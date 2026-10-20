//! Durable routing (`hub-server --durable`): pull every agent send from the
//! shared `router` consumer on the ingress stream instead of a core
//! subscription, republish it, and ack only once the destination has it.
//!
//! - Destinations captured by a stream (inbox, task/session/wave channels)
//!   are republished with a JetStream publish carrying
//!   `Nats-Msg-Id = hub-router:<from>:<meta.id>`; the ingress message is acked
//!   after the store's ack. A redelivered ingress message (a router crashed
//!   between publish and ack) is dropped by the stream's duplicate window,
//!   and live subscribers dedupe by `meta.id`.
//! - Broadcast destinations (`channel.<name>`) stay core-NATS.
//! - Every hub-server pulls from the same durable consumer, so each
//!   envelope is routed by exactly one of them.
//! - Messages that are replays of stored envelopes are dropped
//!   ([`crate::delivery::ingress_rejection`]).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_nats::jetstream::{self, consumer::PullConsumer, AckKind};
use tracing::{debug, error, info, warn};

use super::ControlPlane;
use crate::delivery::{self, setup, DeliveryConfig};

/// Back-off before a failed republish is retried (nak delay).
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// JetStream objects the durable router works on.
pub(super) struct DurableIngress {
    pub(super) js: jetstream::Context,
    pub(super) cfg: Arc<DeliveryConfig>,
    consumer: PullConsumer,
}

impl ControlPlane {
    /// Create/update the streams and the shared consumer, and start
    /// answering inbox attach requests.
    pub(super) async fn start_durable(&self, cfg: Arc<DeliveryConfig>) -> Result<DurableIngress> {
        let js = jetstream::new(self.nats.clone());
        setup::ensure_streams(&js, &cfg)
            .await
            .context("durable delivery: stream setup failed (is JetStream enabled?)")?;
        let consumer = setup::ensure_router_consumer(&js, &cfg).await?;
        let (nats, js2, cfg2) = (self.nats.clone(), js.clone(), cfg.clone());
        tokio::spawn(async move {
            if let Err(e) = delivery::serve_attach(nats, js2, cfg2).await {
                error!(error = %e, "inbox attach responder stopped");
            }
        });
        info!(
            ingress = %cfg.ingress.name,
            inbox = %cfg.inbox.name,
            tasks = %cfg.tasks.name,
            consumer = %cfg.router_consumer,
            "durable delivery on: routing from the ingress stream"
        );
        Ok(DurableIngress { js, cfg, consumer })
    }

    /// Open (or re-open) the pull stream on the shared ingress consumer.
    pub(super) async fn ingress_messages(
        &self,
        ingress: &DurableIngress,
    ) -> Result<jetstream::consumer::pull::Stream> {
        ingress
            .consumer
            .messages()
            .await
            .map_err(|e| anyhow!("ingress consumer: {e}"))
    }

    /// Route one pulled ingress message; ack, nak or term it.
    pub(super) async fn handle_durable(&self, ingress: &DurableIngress, msg: jetstream::Message) {
        let subject = msg.subject.as_str();
        if let Some(why) = delivery::ingress_rejection(subject, msg.headers.as_ref()) {
            warn!(%subject, why, "durable router: dropping replayed message");
            ack(&msg, AckKind::Term).await;
            return;
        }
        let Some((dest, env, bytes)) = self.core.on_send(subject, &msg.payload) else {
            ack(&msg, AckKind::Term).await; // undecodable / refused: never retry
            return;
        };
        let stored = match ingress.cfg.stream_for(&dest) {
            Some(_) => {
                let mut headers = async_nats::HeaderMap::new();
                headers.insert(
                    delivery::MSG_ID_HEADER,
                    delivery::router_msg_id(&env.meta.from, &env.meta.id).as_str(),
                );
                match ingress
                    .js
                    .publish_with_headers(dest.clone(), headers, bytes.into())
                    .await
                {
                    Ok(ack_fut) => ack_fut.await.map(|a| a.duplicate).map_err(|e| anyhow!(e)),
                    Err(e) => Err(anyhow!(e)),
                }
            }
            None => self
                .nats
                .publish(dest.clone(), bytes.into())
                .await
                .map(|()| false)
                .map_err(|e| anyhow!(e)),
        };
        match stored {
            Ok(duplicate) => {
                if duplicate {
                    debug!(%dest, id = %env.meta.id, "redelivered envelope already stored");
                } else {
                    self.core.mirror_envelope(env);
                }
                ack(&msg, AckKind::Ack).await;
            }
            Err(e) => {
                error!(%dest, error = %e, "durable router: republish failed; will retry");
                ack(&msg, AckKind::Nak(Some(RETRY_DELAY))).await;
            }
        }
    }
}

async fn ack(msg: &jetstream::Message, kind: AckKind) {
    if let Err(e) = msg.ack_with(kind).await {
        debug!(error = %e, "ingress ack failed (message will be redelivered)");
    }
}
