//! Accept-policy rejection matching and worker-side policy evaluation.
use crate::{
    manifest::{AcceptPolicy, ManifestRecord},
    protocol::{Envelope, Origin},
};

pub const REJECTED_ACTION: &str = "rejected";
pub const EXIT_REFUSED: i32 = 5;

pub fn valid_message_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

pub fn is_rejection<'a>(env: &'a Envelope, sent_id: &str, identity: &str) -> Option<&'a str> {
    (env.meta.from == "hub-server"
        && env.payload["refused_sender"] == identity
        && env.payload["message_id"] == sent_id
        && env.payload["action"] == REJECTED_ACTION
        && env.meta.reply_to.as_deref() == Some(sent_id))
    .then(|| env.payload["error"].as_str())
    .flatten()
}
pub fn is_rejected_terminal<'a>(
    env: &'a Envelope,
    task_id: &str,
    identity: &str,
) -> Option<&'a str> {
    (env.meta.from == "hub-server"
        && env.payload["refused_sender"] == identity
        && env.payload["message_id"] == task_id
        && env.payload["rejected"] == true
        && env.payload["task_id"] == task_id)
        .then(|| env.payload["error"].as_str())
        .flatten()
}

pub fn task_channel(channel: &str) -> bool {
    if !channel.split('.').all(crate::protocol::valid_identity) {
        return false;
    }
    let parts: Vec<_> = channel.split('.').collect();
    matches!(parts.as_slice(), ["task", x] if !x.is_empty())
        || matches!(parts.as_slice(), ["wave", w, "task", t] if !w.is_empty() && !t.is_empty())
}

/// Only use with router-stamped provenance received over a trusted transport.
pub fn worker_allows(record: &ManifestRecord, origin: Option<&Origin>) -> bool {
    let Some(origin) = origin else {
        return false;
    };
    let user = if origin.identity.as_deref() == Some("hub-orchestrator") {
        origin.on_behalf_of.as_ref().and_then(|o| o.user.as_deref())
    } else {
        origin.user.as_deref()
    };
    match record.effective_accept() {
        AcceptPolicy::Owner => user.is_some() && user == record.user.as_deref(),
        AcceptPolicy::Users { users } => user.is_some_and(|u| users.iter().any(|s| s == u)),
        AcceptPolicy::Tenant => {
            origin.owner_kind.is_some()
                || (origin.identity.as_deref() == Some("hub-orchestrator")
                    && origin
                        .on_behalf_of
                        .as_ref()
                        .is_some_and(|o| !o.identity.is_empty()))
        }
    }
}

/// Worker-local cache, refreshed independently of executing tasks.
pub struct WorkerPolicy(tokio::sync::RwLock<Option<ManifestRecord>>);
impl Default for WorkerPolicy {
    fn default() -> Self {
        Self::new()
    }
}
impl WorkerPolicy {
    pub const fn new() -> Self {
        Self(tokio::sync::RwLock::const_new(None))
    }
    pub async fn refresh(&self, api: &crate::ApiClient) {
        if let Ok(data) = api.request("manifest.self", serde_json::json!({})).await {
            if data["record"].is_null() {
                *self.0.write().await = None;
            } else if let Ok(record) = serde_json::from_value(data["record"].clone()) {
                *self.0.write().await = Some(record);
            }
        }
    }
    pub async fn check(&self, client: &crate::HubClient, env: &Envelope) -> bool {
        let allowed = self
            .0
            .read()
            .await
            .as_ref()
            .map_or(true, |r| worker_allows(r, env.meta.origin.as_ref()));
        if allowed {
            return true;
        }
        let channel = env
            .payload
            .get("task_channel")
            .and_then(|v| v.as_str())
            .unwrap_or(&env.meta.channel);
        if task_channel(channel) {
            let task_id = env
                .payload
                .get("task_id")
                .and_then(|v| v.as_str())
                .or_else(|| env.payload.get("session_id").and_then(|v| v.as_str()))
                .filter(|id| valid_message_id(id))
                .unwrap_or(&env.meta.id);
            let mut result = Envelope::new(
                client.identity(),
                channel,
                crate::MessageKind::Message,
                serde_json::json!({"status":"error", "task_id":task_id, "result":null, "error":"refused by worker accept policy", "rejected":true}),
            );
            result.meta.reply_to = Some(env.meta.id.clone());
            let _ = client.send(&result).await;
        }
        false
    }
}
