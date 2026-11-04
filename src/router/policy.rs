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
            let cleanup = bound == Some("hub-orchestrator")
                && matches!(
                    env.payload["action"].as_str(),
                    Some("cancel" | "session_close")
                );
            let decision = if cleanup { Decision::Allow } else { decision };
            let reason = match decision {
                Decision::Allow => None,
                Decision::Refuse(reason) => Some(reason),
                Decision::NeedsReplyLookup(id) => {
                    let effective = accept::effective_sender(sender(), env.meta.origin.as_ref());
                    let key = (
                        id.clone(),
                        target.to_owned(),
                        effective.unwrap_or_default().to_owned(),
                    );
                    let cached = self.core.negative_replies.lock().unwrap().contains(&key);
                    let permitted = !cached
                        && self
                            .core
                            .negative_replies
                            .lock()
                            .unwrap()
                            .take_lookup(bound.unwrap_or_default());
                    let result = if cached || !permitted {
                        None
                    } else if let Some(storage) = &self.storage {
                        reply_lookup(stored_reply_allows(
                            storage.as_ref(),
                            &id,
                            target,
                            effective,
                        ))
                        .await
                    } else {
                        None
                    };
                    let allowed = result == Some(true);
                    if result == Some(false) {
                        self.core.negative_replies.lock().unwrap().insert(key);
                    }
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
            let Some(bytes) = self.core.encode_within_limit("rejection", &mut notice) else {
                continue;
            };
            let duplicate = if let Some(ingress) = durable {
                // Stream deduplication does not suppress live core subscribers.
                // Avoid republishing notices already acknowledged in this process.
                if self
                    .core
                    .published_notices
                    .lock()
                    .unwrap()
                    .contains(&notice.meta.id, ingress.cfg.duplicate_window_secs)
                {
                    continue;
                }
                let mut headers = async_nats::HeaderMap::new();
                let id = crate::delivery::router_msg_id("hub-server", &notice.meta.id);
                headers.insert(
                    crate::delivery::MSG_ID_HEADER,
                    id.parse::<async_nats::HeaderValue>()?,
                );
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
            if durable.is_some() {
                self.core
                    .published_notices
                    .lock()
                    .unwrap()
                    .record(notice.meta.id.clone());
            }
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
                "target": target, "error":error, "refused_sender":sender,
            }),
        )
        .to(sender);
        notice.meta.id = format!("rej:{sender}:{}", refused.meta.id);
        notices.push(notice);
    }
    let channel = refused
        .payload
        .get("task_channel")
        .and_then(|v| v.as_str())
        .unwrap_or(&refused.meta.channel);
    if let Some(sender) = bound.filter(|_| nats_hub_client::accept::task_channel(channel)) {
        let task_id = refused
            .payload
            .get("task_id")
            .and_then(|v| v.as_str())
            .or_else(|| refused.payload.get("session_id").and_then(|v| v.as_str()))
            .filter(|id| nats_hub_client::accept::valid_message_id(id))
            .unwrap_or(&refused.meta.id);
        let mut terminal = Envelope::new(
            "hub-server",
            channel,
            MessageKind::Message,
            json!({
                "message_id":refused.meta.id, "refused_sender":sender,
                "status":"error", "task_id":task_id, "result":null, "error":error, "rejected":true,
            }),
        );
        terminal.meta.id = format!("rej-term:{sender}:{}", refused.meta.id);
        notices.push(terminal);
    }
    notices
}

async fn reply_lookup(lookup: impl std::future::Future<Output = Result<bool>>) -> Option<bool> {
    tokio::time::timeout(std::time::Duration::from_millis(250), lookup)
        .await
        .ok()
        .and_then(Result::ok)
}

async fn stored_reply_allows(
    storage: &dyn crate::Storage,
    id: &str,
    target: &str,
    sender: Option<&str>,
) -> Result<bool> {
    Ok(storage
        .get_envelope(id)
        .await?
        .as_ref()
        .is_some_and(|old| stored_pair_allows(old, target, sender)))
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
    #[test]
    fn notices_are_sender_scoped_and_echo_only_bounded_task_ids() {
        let env = Envelope::new(
            "untrusted",
            "task.safe",
            MessageKind::Message,
            json!({"task_id":"bad\r\nid"}),
        )
        .to("worker");
        assert!(rejection_notices(&env, None, accept::POLICY).is_empty());
        let a = rejection_notices(&env, Some("alice"), accept::POLICY);
        let b = rejection_notices(&env, Some("bob"), accept::POLICY);
        assert_ne!(a[0].meta.id, b[0].meta.id);
        assert_ne!(a[1].meta.id, b[1].meta.id);
        assert_eq!(a[1].payload["task_id"], env.meta.id);
        assert_eq!(a[1].payload["refused_sender"], "alice");
        assert_eq!(a[1].payload["message_id"], env.meta.id);
    }

    #[tokio::test]
    async fn transient_lookup_failures_are_not_cacheable_denials() {
        assert_eq!(
            reply_lookup(async { Err(anyhow::anyhow!("unavailable")) }).await,
            None
        );
        assert_eq!(reply_lookup(std::future::pending()).await, None);
        assert_eq!(reply_lookup(async { Ok(false) }).await, Some(false));
        assert_eq!(reply_lookup(async { Ok(true) }).await, Some(true));
    }

    #[test]
    fn notice_ids_cannot_alias_identity_and_message_boundaries() {
        let mut env = Envelope::new("a", "task.x", MessageKind::Message, json!({})).to("worker");
        env.meta.id = "b-c".into();
        let first = rejection_notices(&env, Some("a"), accept::POLICY);
        env.meta.id = "c".into();
        let second = rejection_notices(&env, Some("a-b"), accept::POLICY);
        assert_ne!(first[0].meta.id, second[0].meta.id);
        assert_ne!(first[1].meta.id, second[1].meta.id);
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
        assert!(
            stored_reply_allows(&storage, &old.meta.id, "target", Some("sender"))
                .await
                .unwrap()
        );
        assert!(
            !stored_reply_allows(&storage, &old.meta.id, "target", Some("stranger"))
                .await
                .unwrap()
        );
        let mut row = storage.get_envelope(&old.meta.id).await.unwrap().unwrap();
        row.stored_at = chrono::Utc::now() - chrono::Duration::hours(25);
        assert!(!stored_pair_allows(&row, "target", Some("sender")));
        assert!(
            !stored_reply_allows(&storage, "missing", "target", Some("sender"))
                .await
                .unwrap()
        );
    }
}
