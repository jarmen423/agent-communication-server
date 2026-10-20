//! Stream and consumer setup. Only hub-server runs this (agents may not
//! create JetStream objects; see `docs/DELIVERY.md` → Permissions).
//!
//! Every call is idempotent: a stream that exists is updated to the
//! configured limits, a consumer that exists is updated in place, so two
//! hub-servers starting at once converge on the same objects.

use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use async_nats::jetstream::{
    self,
    consumer::{pull, AckPolicy, DeliverPolicy},
    context::CreateStreamErrorKind,
    stream::{self, DiscardPolicy, RetentionPolicy},
};
use tracing::info;

use super::config::{
    DeliveryConfig, StorageKind, StreamLimits, INBOX_SUBJECTS, INGRESS_SUBJECTS, TASKS_SUBJECTS,
};
use super::inbox_consumer_name;
use crate::protocol::subjects;

fn stream_config(
    limits: &StreamLimits,
    subjects: &[&str],
    retention: RetentionPolicy,
    cfg: &DeliveryConfig,
    description: &str,
    allow_direct: bool,
) -> stream::Config {
    stream::Config {
        name: limits.name.clone(),
        description: Some(description.to_string()),
        subjects: subjects.iter().map(|s| s.to_string()).collect(),
        retention,
        max_age: limits.max_age(),
        max_bytes: limits.max_bytes,
        max_messages: limits.max_msgs,
        max_messages_per_subject: limits.max_msgs_per_subject,
        max_message_size: limits.max_msg_size,
        discard: DiscardPolicy::Old,
        num_replicas: limits.replicas.max(1),
        storage: match limits.storage {
            StorageKind::File => stream::StorageType::File,
            StorageKind::Memory => stream::StorageType::Memory,
        },
        duplicate_window: Duration::from_secs(cfg.duplicate_window_secs),
        // Only the tasks stream: catch-up reads it by sequence via
        // DIRECT.GET. Inbox/ingress are read through consumers only.
        allow_direct,
        ..Default::default()
    }
}

/// The three stream configs, in creation order.
pub fn stream_configs(cfg: &DeliveryConfig) -> [stream::Config; 3] {
    [
        stream_config(
            &cfg.ingress,
            INGRESS_SUBJECTS,
            RetentionPolicy::WorkQueue,
            cfg,
            "nats-hub ingress: every agent send, until the router acks it",
            false,
        ),
        stream_config(
            &cfg.inbox,
            INBOX_SUBJECTS,
            RetentionPolicy::Limits,
            cfg,
            "nats-hub DMs, kept for offline recipients",
            false,
        ),
        stream_config(
            &cfg.tasks,
            TASKS_SUBJECTS,
            RetentionPolicy::Limits,
            cfg,
            "nats-hub task/session/wave channels, kept for resume",
            true,
        ),
    ]
}

/// Create the streams, or update them to the configured limits.
pub async fn ensure_streams(js: &jetstream::Context, cfg: &DeliveryConfig) -> Result<()> {
    for sc in stream_configs(cfg) {
        let name = sc.name.clone();
        match js.update_stream(&sc).await {
            Ok(_) => info!(stream = %name, "durable delivery: stream updated"),
            Err(e) if is_not_found(e.kind()) => {
                js.create_stream(sc)
                    .await
                    .map_err(|e| anyhow!("{e}"))
                    .with_context(|| format!("creating stream {name}"))?;
                info!(stream = %name, "durable delivery: stream created");
            }
            Err(e) => {
                return Err(anyhow!("{e}")).with_context(|| format!("updating stream {name}"))
            }
        }
    }
    Ok(())
}

fn is_not_found(kind: CreateStreamErrorKind) -> bool {
    matches!(kind, CreateStreamErrorKind::JetStream(ref e)
        if e.error_code() == jetstream::ErrorCode::STREAM_NOT_FOUND)
}

/// The shared durable pull consumer all hub-servers route from.
pub fn router_consumer_config(cfg: &DeliveryConfig) -> pull::Config {
    pull::Config {
        durable_name: Some(cfg.router_consumer.clone()),
        name: Some(cfg.router_consumer.clone()),
        description: Some("nats-hub router (shared by every hub-server)".into()),
        ack_policy: AckPolicy::Explicit,
        ack_wait: Duration::from_secs(cfg.ack_wait_secs),
        max_deliver: cfg.router_max_deliver,
        max_ack_pending: cfg.router_max_ack_pending,
        deliver_policy: DeliverPolicy::All,
        ..Default::default()
    }
}

pub async fn ensure_router_consumer(
    js: &jetstream::Context,
    cfg: &DeliveryConfig,
) -> Result<jetstream::consumer::PullConsumer> {
    js.create_consumer_on_stream(router_consumer_config(cfg), &cfg.ingress.name)
        .await
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| {
            format!(
                "creating consumer {} on {}",
                cfg.router_consumer, cfg.ingress.name
            )
        })
}

/// One identity's inbox consumer: pull, explicit ack, every retained DM
/// for `channel.inbox.<identity>` from the start of the retention window.
pub fn inbox_consumer_config(cfg: &DeliveryConfig, identity: &str) -> pull::Config {
    let name = inbox_consumer_name(identity);
    pull::Config {
        durable_name: Some(name.clone()),
        name: Some(name),
        description: Some(format!("nats-hub inbox of {identity}")),
        filter_subject: subjects::inbox(identity),
        ack_policy: AckPolicy::Explicit,
        ack_wait: Duration::from_secs(cfg.ack_wait_secs),
        max_ack_pending: cfg.inbox_max_ack_pending,
        deliver_policy: DeliverPolicy::All,
        inactive_threshold: Duration::from_secs(cfg.inbox_inactive_secs),
        ..Default::default()
    }
}

/// Create (or update) `inbox-<identity>` on the inbox stream.
pub async fn ensure_inbox_consumer(
    js: &jetstream::Context,
    cfg: &DeliveryConfig,
    identity: &str,
) -> Result<()> {
    crate::protocol::require_valid_identity(identity)?;
    js.create_consumer_on_stream(inbox_consumer_config(cfg, identity), &cfg.inbox.name)
        .await
        .map_err(|e| anyhow!("{e}"))
        .with_context(|| format!("creating inbox consumer for {identity}"))?;
    Ok(())
}
