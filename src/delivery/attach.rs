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
use std::time::Duration;

use anyhow::Result;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::config::DeliveryConfig;
use super::inbox_consumer_name;
use super::setup::ensure_inbox_consumer;
use crate::protocol::{subjects, valid_identity};

/// `hub.delivery.<identity>.inbox`.
pub const ATTACH_PREFIX: &str = "hub.delivery";
/// Several hub-servers answer attach requests as one queue group.
pub const ATTACH_QUEUE_GROUP: &str = "hub-delivery";

/// The attach subject for `identity`.
pub fn attach_subject(identity: &str) -> String {
    format!("{ATTACH_PREFIX}.{identity}.inbox")
}

/// Identity token of an attach subject.
fn attach_identity(subject: &str) -> Option<&str> {
    let rest = subject.strip_prefix(ATTACH_PREFIX)?.strip_prefix('.')?;
    let id = rest.strip_suffix(".inbox")?;
    valid_identity(id).then_some(id)
}

/// Reply body of an attach request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttachReply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Where an identity's durable inbox lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxTarget {
    pub stream: String,
    pub consumer: String,
}

/// Outcome of [`attach_inbox`].
#[derive(Debug)]
pub enum Attach {
    /// hub-server is durable and the consumer exists.
    Attached(InboxTarget),
    /// Nobody answers (core-mode hub, or the router is down right now).
    NoResponders,
    /// The request failed otherwise (timeout, permission, hub error).
    Failed(String),
}

/// Ask hub-server for `identity`'s inbox consumer.
pub async fn attach_inbox(nats: &async_nats::Client, identity: &str, timeout: Duration) -> Attach {
    let req = nats.request(attach_subject(identity), "{}".into());
    let msg = match tokio::time::timeout(timeout, req).await {
        Err(_) => return Attach::Failed("attach timed out".into()),
        Ok(Err(e)) if e.kind() == async_nats::client::RequestErrorKind::NoResponders => {
            return Attach::NoResponders
        }
        Ok(Err(e)) => return Attach::Failed(e.to_string()),
        Ok(Ok(msg)) => msg,
    };
    match serde_json::from_slice::<AttachReply>(&msg.payload) {
        Ok(AttachReply {
            ok: true,
            stream: Some(stream),
            consumer: Some(consumer),
            ..
        }) => Attach::Attached(InboxTarget { stream, consumer }),
        Ok(r) => Attach::Failed(r.error.unwrap_or_else(|| "attach refused".into())),
        Err(e) => Attach::Failed(format!("bad attach reply: {e}")),
    }
}

/// hub-server side: answer attach requests until the subscription ends.
pub async fn serve_attach(
    nats: async_nats::Client,
    js: async_nats::jetstream::Context,
    cfg: Arc<DeliveryConfig>,
) -> Result<()> {
    let mut sub = nats
        .queue_subscribe(
            format!("{ATTACH_PREFIX}.*.inbox"),
            ATTACH_QUEUE_GROUP.to_string(),
        )
        .await?;
    info!("durable delivery: answering inbox attach on {ATTACH_PREFIX}.*.inbox");
    while let Some(msg) = sub.next().await {
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
