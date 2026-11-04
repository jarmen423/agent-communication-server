//! Pure accept decisions and a bounded index of delivered DMs.
use crate::{
    manifest::stamp::Sender,
    manifest::{AcceptPolicy, IdentityManifest},
    protocol::{Envelope, Origin},
};
use chrono::{DateTime, Utc};
use std::{
    collections::{HashMap, VecDeque},
    sync::Mutex,
};

pub(super) const TTL: i64 = 24 * 60 * 60;
const CAPACITY: usize = 200_000;
#[derive(Debug, PartialEq)]
pub(super) enum Decision {
    Allow,
    Refuse(&'static str),
    NeedsReplyLookup(String),
}
pub(super) const POLICY: &str = "accept_policy";
pub(super) const UNKNOWN: &str = "unknown_target";

#[derive(Default)]
pub(super) struct ReplyIndex {
    entries: HashMap<String, (String, String, DateTime<Utc>)>,
    order: VecDeque<String>,
    bytes: usize,
}
impl ReplyIndex {
    pub(super) fn record(&mut self, env: &Envelope) {
        let Some(to) = &env.meta.to else {
            return;
        };
        if to == "hub-orchestrator" {
            return;
        }
        if env.meta.origin.as_ref().and_then(|o| o.identity.as_deref()) != Some(&env.meta.from) {
            return;
        }
        if self.entries.contains_key(&env.meta.id) {
            return;
        }
        let cost = 2 * env.meta.id.len() + env.meta.from.len() + to.len();
        const MAX_BYTES: usize = 32 * 1024 * 1024;
        if cost > MAX_BYTES {
            return;
        }
        while self.bytes + cost > MAX_BYTES
            || self.entries.len() >= CAPACITY
            || self.order.front().is_some_and(|id| {
                self.entries
                    .get(id)
                    .is_some_and(|(_, _, ts)| (Utc::now() - *ts).num_seconds() > TTL)
            })
        {
            let Some(id) = self.order.pop_front() else {
                break;
            };
            if let Some((from, to, _)) = self.entries.remove(&id) {
                self.bytes -= 2 * id.len() + from.len() + to.len();
            }
        }
        self.bytes += cost;
        self.order.push_back(env.meta.id.clone());
        self.entries.insert(
            env.meta.id.clone(),
            (env.meta.from.clone(), to.clone(), Utc::now()),
        );
    }
    fn permits(&self, id: &str, target: &str, sender: &str) -> bool {
        self.entries.get(id).is_some_and(|(from, to, ts)| {
            from == target && to == sender && (Utc::now() - *ts).num_seconds() <= TTL
        })
    }
}
#[derive(Default)]
pub(super) struct NegativeReplies {
    entries: HashMap<(String, String, String), std::time::Instant>,
    order: VecDeque<(String, String, String)>,
}
impl NegativeReplies {
    pub(super) fn contains(&self, key: &(String, String, String)) -> bool {
        self.entries
            .get(key)
            .is_some_and(|at| at.elapsed().as_secs() < 60)
    }
    pub(super) fn insert(&mut self, key: (String, String, String)) {
        if key.0.len() + key.1.len() + key.2.len() > 512 {
            return;
        }
        if let Some(at) = self.entries.get_mut(&key) {
            *at = std::time::Instant::now();
            return;
        }
        while self.entries.len() >= 10_000 {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, std::time::Instant::now());
    }
}
#[derive(Default)]
pub(super) struct PublishedNotices {
    entries: HashMap<String, std::time::Instant>,
    order: VecDeque<String>,
}
impl PublishedNotices {
    pub(super) fn contains(&self, id: &str, window: u64) -> bool {
        self.entries
            .get(id)
            .is_some_and(|at| at.elapsed().as_secs() < window)
    }
    pub(super) fn record(&mut self, id: String) {
        if id.len() > 1024 {
            return;
        }
        if let Some(at) = self.entries.get_mut(&id) {
            *at = std::time::Instant::now();
            return;
        }
        while self.entries.len() >= 10_000 {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }
        self.order.push_back(id.clone());
        self.entries.insert(id, std::time::Instant::now());
    }
}
pub(super) type SharedReplies = Mutex<ReplyIndex>;

pub(super) fn effective_sender<'a>(
    sender: Sender<'a>,
    origin: Option<&'a Origin>,
) -> Option<&'a str> {
    match sender {
        Sender::Bound("hub-orchestrator") => origin
            .and_then(|o| o.on_behalf_of.as_ref())
            .map(|a| a.identity.as_str()),
        Sender::Bound(id) => Some(id),
        Sender::Legacy => None,
    }
}
pub(super) fn decide(
    manifest: &IdentityManifest,
    sender: Sender<'_>,
    origin: Option<&Origin>,
    target: &str,
    reply_to: Option<&str>,
    replies: &ReplyIndex,
) -> Decision {
    if !manifest.writer_configured() || target == "hub-orchestrator" {
        return Decision::Allow;
    }
    let Some(target_record) = manifest.get(target) else {
        return Decision::Refuse(UNKNOWN);
    };
    let effective = effective_sender(sender, origin);
    let sender_record = effective.and_then(|id| manifest.get(id));
    let user = sender_record.as_ref().and_then(|r| r.user.as_deref());
    let allowed = match target_record.effective_accept() {
        AcceptPolicy::Owner => {
            if target_record.user.is_none() {
                tracing::debug!("tenant-owned identity has owner accept policy");
            }
            user.is_some() && user == target_record.user.as_deref()
        }
        AcceptPolicy::Users { users } => user.is_some_and(|u| users.iter().any(|s| s == u)),
        AcceptPolicy::Tenant => sender_record.is_some(),
    };
    if allowed {
        return Decision::Allow;
    }
    if let (Some(sender), Some(id)) = (effective, reply_to) {
        if replies.permits(id, target, sender) {
            return Decision::Allow;
        }
        return Decision::NeedsReplyLookup(id.to_string());
    }
    Decision::Refuse(POLICY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::ManifestRecord;
    use serde_json::json;
    fn record(id: &str, user: Option<&str>, policy: serde_json::Value) -> ManifestRecord {
        serde_json::from_value(json!({"identity":id,"owner_kind":if user.is_some(){"user"}else{"tenant"},"user":user,"agent":"echo","accept":policy,"rev":1})).unwrap()
    }
    async fn manifest(writer: bool) -> IdentityManifest {
        let m = IdentityManifest::new(if writer {
            ["writer".into()].into()
        } else {
            Default::default()
        });
        for r in [
            record("alice-w", Some("alice"), json!({"from":"owner"})),
            record("alice", Some("alice"), json!({"from":"tenant"})),
            record("bob", Some("bob"), json!({"from":"tenant"})),
            record("service", None, json!({"from":"tenant"})),
        ] {
            m.put(None, r).await.unwrap();
        }
        m
    }
    #[test]
    fn reply_index_bounds_bytes_and_excludes_service_inbox() {
        let mut index = ReplyIndex::default();
        let mut env = Envelope::new(
            "alice",
            "chat",
            crate::protocol::MessageKind::Message,
            json!({}),
        )
        .to("hub-orchestrator");
        env.meta.origin = Some(Origin {
            identity: Some("alice".into()),
            ..Default::default()
        });
        index.record(&env);
        assert!(index.entries.is_empty());
        env.meta.to = Some("w".repeat(1024 * 1024));
        for n in 0..40 {
            env.meta.id = format!("id-{n}");
            index.record(&env);
        }
        assert!(index.bytes <= 32 * 1024 * 1024);
        assert!(index.entries.len() < 40);
        assert!(!index.entries.contains_key("id-0"));
    }

    #[tokio::test]
    async fn owner_bound_legacy_unknown_and_no_writer() {
        let m = manifest(true).await;
        let idx = ReplyIndex::default();
        for (sender, target, expected) in [
            (Sender::Bound("alice"), "alice-w", Decision::Allow),
            (Sender::Bound("bob"), "alice-w", Decision::Refuse(POLICY)),
            (Sender::Legacy, "alice-w", Decision::Refuse(POLICY)),
            (Sender::Bound("alice"), "missing", Decision::Refuse(UNKNOWN)),
            (Sender::Legacy, "hub-orchestrator", Decision::Allow),
            (Sender::Bound("service"), "bob", Decision::Allow),
        ] {
            assert_eq!(decide(&m, sender, None, target, None, &idx), expected);
        }
        let empty = IdentityManifest::new(["writer".into()].into());
        assert_eq!(
            decide(&empty, Sender::Bound("alice"), None, "alice-w", None, &idx),
            Decision::Refuse(UNKNOWN)
        );
        assert_eq!(
            decide(
                &manifest(false).await,
                Sender::Legacy,
                None,
                "missing",
                None,
                &idx
            ),
            Decision::Allow
        );
    }
    #[tokio::test]
    async fn reply_requires_exact_pair_and_live_target() {
        let m = manifest(true).await;
        let mut idx = ReplyIndex::default();
        let mut dm = Envelope::new(
            "alice-w",
            "chat",
            crate::protocol::MessageKind::Message,
            json!({}),
        )
        .to("bob");
        dm.meta.origin = Some(Origin {
            identity: Some("alice-w".into()),
            ..Default::default()
        });
        idx.record(&dm);
        assert_eq!(
            decide(
                &m,
                Sender::Bound("bob"),
                None,
                "alice-w",
                Some(&dm.meta.id),
                &idx
            ),
            Decision::Allow
        );
        assert!(matches!(
            decide(
                &m,
                Sender::Bound("service"),
                None,
                "alice-w",
                Some(&dm.meta.id),
                &idx
            ),
            Decision::NeedsReplyLookup(_)
        ));
        m.remove(None, "alice-w", 2).await.unwrap();
        assert_eq!(
            decide(
                &m,
                Sender::Bound("bob"),
                None,
                "alice-w",
                Some(&dm.meta.id),
                &idx
            ),
            Decision::Refuse(UNKNOWN)
        );
    }
    #[tokio::test]
    async fn wave_effective_sender_is_the_creator() {
        let m = manifest(true).await;
        let origin = Origin {
            on_behalf_of: Some(crate::protocol::OnBehalfOf {
                identity: "alice".into(),
                user: None,
            }),
            ..Default::default()
        };
        assert_eq!(
            decide(
                &m,
                Sender::Bound("hub-orchestrator"),
                Some(&origin),
                "alice-w",
                None,
                &ReplyIndex::default()
            ),
            Decision::Allow
        );
        assert_eq!(
            decide(
                &m,
                Sender::Bound("bob"),
                Some(&origin),
                "alice-w",
                None,
                &ReplyIndex::default()
            ),
            Decision::Refuse(POLICY)
        );
    }
}
