//! Async reply fallback and publication of bounded rejection notices.
use super::{
    accept::{self, Decision},
    bound_send_subject,
    durable::DurableIngress,
    ControlPlane,
};
use crate::{
    manifest::stamp::Sender,
    protocol::{Envelope, MessageKind},
};
use anyhow::Result;
use serde_json::json;

// Keep the routed envelope inline on the hot path.
#[allow(clippy::large_enum_variant)]
pub(super) enum SendOutcome {
    Routed(String, Envelope, Vec<u8>),
    Rejected,
    Dropped,
}

impl ControlPlane {
    /// A rejection is handled successfully (`SendOutcome::Rejected`); only notice publication
    /// failures are retried by durable ingress.
    pub(super) async fn accept_send(
        &self,
        subject: &str,
        payload: &[u8],
        durable: Option<&DurableIngress>,
    ) -> Result<SendOutcome> {
        let Some(env) = self.core.prepare_send(subject, payload) else {
            return Ok(SendOutcome::Dropped);
        };
        let bound = bound_send_subject(subject).map(|(id, _)| id);
        if let Some(target) = env.meta.to.as_deref() {
            let sender = || bound.map(Sender::Bound).unwrap_or(Sender::Legacy);
            let decision = accept::decide(
                &self.core.manifest,
                sender(),
                env.meta.origin.as_ref(),
                target,
                env.meta.reply_to.as_deref(),
                &self.core.replies.lock().unwrap(),
            );
            let reason = match decision {
                Decision::Allow => None,
                Decision::Refuse(reason) => Some(reason),
                Decision::NeedsReplyLookup(id) => {
                    let effective = accept::effective_sender(sender(), env.meta.origin.as_ref());
                    let allowed = if let Some(storage) = &self.storage {
                        stored_reply_allows(storage.as_ref(), &id, target, effective).await
                    } else {
                        false
                    };
                    (!allowed).then_some(accept::POLICY)
                }
            };
            if let Some(reason) = reason {
                if let Some(metrics) = &self.core.metrics {
                    metrics
                        .accept_refused_total
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                self.reject(&env, bound, reason, durable).await?;
                return Ok(SendOutcome::Rejected);
            }
        }
        Ok(match self.core.finish_send(subject, env) {
            Some((dest, env, bytes)) => SendOutcome::Routed(dest, env, bytes),
            None => SendOutcome::Dropped,
        })
    }

    async fn reject(
        &self,
        refused: &Envelope,
        bound: Option<&str>,
        reason: &str,
        durable: Option<&DurableIngress>,
    ) -> Result<()> {
        let notices = rejection_notices(refused, bound, reason);
        for mut notice in notices {
            notice.meta.reply_to = Some(refused.meta.id.clone());
            let dest = format!("channel.{}", notice.meta.channel);
            let bytes = notice.to_json_bytes()?;
            let duplicate = if let Some(ingress) = durable {
                let mut headers = async_nats::HeaderMap::new();
                let id = crate::delivery::router_msg_id("hub-server", &notice.meta.id);
                headers.insert(crate::delivery::MSG_ID_HEADER, id.as_str());
                ingress
                    .js
                    .publish_with_headers(dest, headers, bytes.into())
                    .await?
                    .await?
                    .duplicate
            } else {
                self.nats.publish(dest, bytes.into()).await?;
                false
            };
            if !duplicate {
                self.core.emit_ws(&notice);
                self.core.mirror_envelope(notice);
            }
        }
        Ok(())
    }
}

fn rejection_notices(refused: &Envelope, bound: Option<&str>, reason: &str) -> Vec<Envelope> {
    let target = refused.meta.to.as_deref().unwrap_or_default();
    let error = if reason == accept::UNKNOWN {
        format!("{target} is not a known identity on this hub")
    } else {
        format!(
            "{target} does not accept tasks from {}",
            bound.unwrap_or("unknown")
        )
    };
    let mut notices = Vec::new();
    if let Some(sender) = bound {
        let channel = format!("inbox.{sender}");
        let mut notice = Envelope::new(
            "hub-server",
            &channel,
            MessageKind::Control,
            json!({
                "action":"rejected", "reason":"accept_policy", "message_id":refused.meta.id,
                "target": target, "error":error,
            }),
        )
        .to(sender);
        notice.meta.id = format!("rej-{}", refused.meta.id);
        notices.push(notice);
    }
    let channel = refused
        .payload
        .get("task_channel")
        .and_then(|v| v.as_str())
        .unwrap_or(&refused.meta.channel);
    if nats_hub_client::accept::task_channel(channel) {
        let task_id = refused
            .payload
            .get("task_id")
            .and_then(|v| v.as_str())
            .or_else(|| refused.payload.get("session_id").and_then(|v| v.as_str()))
            .unwrap_or(&refused.meta.id);
        let mut terminal = Envelope::new(
            "hub-server",
            channel,
            MessageKind::Message,
            json!({
                "status":"error", "task_id":task_id, "result":null, "error":error, "rejected":true,
            }),
        );
        terminal.meta.id = format!("rej-term-{}", refused.meta.id);
        notices.push(terminal);
    }
    notices
}

async fn stored_reply_allows(
    storage: &dyn crate::Storage,
    id: &str,
    target: &str,
    sender: Option<&str>,
) -> bool {
    match storage.get_envelope(id).await {
        Ok(Some(old)) => stored_pair_allows(&old, target, sender),
        _ => false,
    }
}

fn stored_pair_allows(
    old: &crate::storage::EnvelopeRecord,
    target: &str,
    sender: Option<&str>,
) -> bool {
    sender.is_some()
        && old.from_identity == target
        && old.to_identity.as_deref() == sender
        && old.origin.as_ref().and_then(|o| o.identity.as_deref()) == Some(target)
        && (0..=accept::TTL).contains(&(chrono::Utc::now() - old.stored_at).num_seconds())
}

#[cfg(all(test, feature = "storage-surreal"))]
mod tests {
    use super::*;
    use crate::{Storage, SurrealStorage};
    #[test]
    fn notices_never_dm_a_self_asserted_sender_or_publish_to_an_inbox_channel() {
        let env = Envelope::new(
            "victim",
            "task.safe",
            MessageKind::Message,
            json!({"task_channel":"inbox.victim"}),
        )
        .to("worker");
        assert!(rejection_notices(&env, None, accept::POLICY).is_empty());
        let notices = rejection_notices(&env, Some("bound"), accept::POLICY);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].meta.to.as_deref(), Some("bound"));
        assert_eq!(notices[0].meta.channel, "inbox.bound");
    }
    #[tokio::test]
    async fn storage_fallback_checks_pair_provenance_and_age() {
        let storage = SurrealStorage::connect_memory().await.unwrap();
        storage.migrate().await.unwrap();
        let mut old = Envelope::new("target", "chat", MessageKind::Message, json!({})).to("sender");
        old.meta.origin = Some(crate::protocol::Origin {
            identity: Some("target".into()),
            ..Default::default()
        });
        storage.store_envelope(&old).await.unwrap();
        assert!(stored_reply_allows(&storage, &old.meta.id, "target", Some("sender")).await);
        assert!(!stored_reply_allows(&storage, &old.meta.id, "target", Some("stranger")).await);
        let mut row = storage.get_envelope(&old.meta.id).await.unwrap().unwrap();
        row.stored_at = chrono::Utc::now() - chrono::Duration::hours(25);
        assert!(!stored_pair_allows(&row, "target", Some("sender")));
        assert!(!stored_reply_allows(&storage, "missing", "target", Some("sender")).await);
    }
}
