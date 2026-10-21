//! Request attachment to a hub-managed durable inbox consumer.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// `hub.delivery.<identity>.inbox`.
pub const ATTACH_PREFIX: &str = "hub.delivery";
/// Several hub-servers answer attach requests as one queue group.
pub const ATTACH_QUEUE_GROUP: &str = "hub-delivery";

/// The attach subject for `identity`.
pub fn attach_subject(identity: &str) -> String {
    format!("{ATTACH_PREFIX}.{identity}.inbox")
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
