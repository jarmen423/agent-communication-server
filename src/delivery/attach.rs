//! Inbox attach: an agent asks hub-server for its durable inbox consumer.
//!
//! Request `hub.delivery.<identity>.inbox` (any body). hub-server creates or
//! updates `inbox-<identity>` (filtered to `channel.inbox.<identity>`) and
//! replies `{"ok": true, "stream", "consumer", "filter"}`. On a bound hub the
//! subject is the identity proof: hub-admin lets each user publish
//! `hub.delivery.<own id>.>` only. A hub without `--durable` doesn't
//! subscribe, so the request fails fast with "no responders" and the client
//! keeps its plain core-NATS subscription.

use std::sync::Arc;

use anyhow::Result;
use futures_util::StreamExt;
use tracing::{debug, info, warn};

pub use nats_hub_client::delivery::attach::*;

use super::config::DeliveryConfig;
use super::inbox_consumer_name;
use super::setup::ensure_inbox_consumer;
use crate::protocol::{subjects, valid_identity};

/// Identity token of an attach subject.
fn attach_identity(subject: &str) -> Option<&str> {
    let rest = subject.strip_prefix(ATTACH_PREFIX)?.strip_prefix('.')?;
    let id = rest.strip_suffix(".inbox")?;
    valid_identity(id).then_some(id)
}

/// hub-server side: answer attach requests until the subscription ends.
pub async fn serve_attach(
    nats: async_nats::Client,
    js: async_nats::jetstream::Context,
    cfg: Arc<DeliveryConfig>,
) -> Result<()> {
    serve_attach_until(nats, js, cfg, tokio_util::sync::CancellationToken::new()).await
}

/// Managed attach listener: finish the active request before stopping.
pub async fn serve_attach_until(
    nats: async_nats::Client,
    js: async_nats::jetstream::Context,
    cfg: Arc<DeliveryConfig>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let mut sub = nats
        .queue_subscribe(
            format!("{ATTACH_PREFIX}.*.inbox"),
            ATTACH_QUEUE_GROUP.to_string(),
        )
        .await?;
    info!("durable delivery: answering inbox attach on {ATTACH_PREFIX}.*.inbox");
    loop {
        let msg = tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            msg = sub.next() => match msg { Some(msg) => msg, None => break },
        };
        let Some(reply_to) = msg.reply.clone() else {
            continue;
        };
        let reply = match attach_identity(msg.subject.as_str()) {
            None => AttachReply {
                ok: false,
                error: Some("invalid identity in attach subject".into()),
                ..empty()
            },
            Some(id) => match ensure_inbox_consumer(&js, &cfg, id).await {
                Ok(()) => {
                    debug!(identity = %id, "inbox consumer attached");
                    AttachReply {
                        ok: true,
                        stream: Some(cfg.inbox.name.clone()),
                        consumer: Some(inbox_consumer_name(id)),
                        filter: Some(subjects::inbox(id)),
                        error: None,
                    }
                }
                Err(e) => {
                    warn!(identity = %id, error = %format!("{e:#}"), "inbox attach failed");
                    AttachReply {
                        ok: false,
                        error: Some(format!("{e:#}")),
                        ..empty()
                    }
                }
            },
        };
        let body = serde_json::to_vec(&reply).unwrap_or_default();
        if let Err(e) = nats.publish(reply_to, body.into()).await {
            warn!(error = %e, "attach reply failed");
        }
    }
    Ok(())
}

fn empty() -> AttachReply {
    AttachReply {
        ok: false,
        stream: None,
        consumer: None,
        filter: None,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_subject_round_trip() {
        assert_eq!(attach_subject("bob"), "hub.delivery.bob.inbox");
        assert_eq!(attach_identity("hub.delivery.bob.inbox"), Some("bob"));
        assert_eq!(attach_identity("hub.delivery.a.b.inbox"), None);
        assert_eq!(attach_identity("hub.delivery..inbox"), None);
        assert_eq!(attach_identity("hub.delivery.bob.outbox"), None);
    }
}
