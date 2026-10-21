//! Durable-delivery configuration: stream names and per-stream limits.
//!
//! Every field has a default, so `DeliveryConfig::default()` is a complete,
//! working setup for a self-hosted hub. Operators override any subset from a
//! JSON file (`hub-server --durable --delivery-config <path>`); unknown keys
//! are rejected so a typo can't silently fall back to a default.
//!
//! Names and limits are configurable because a hosted hub runs one NATS
//! account per tenant, each with its own JetStream quota: limits must fit
//! inside the account's `max_file`/`max_mem`, and an operator may run more
//! than one hub in one account under different stream names.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Where a stream keeps its messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StorageKind {
    File,
    Memory,
}

/// Name + limits of one stream. `-1` means unlimited for the count/size
/// limits (the JetStream convention).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamLimits {
    pub name: String,
    /// Messages older than this are removed (seconds; 0 = keep forever).
    pub max_age_secs: u64,
    /// Total stream size cap in bytes (oldest discarded first).
    #[serde(default = "unlimited")]
    pub max_bytes: i64,
    /// Total message cap (oldest discarded first).
    #[serde(default = "unlimited")]
    pub max_msgs: i64,
    /// Per-subject message cap (per inbox / per task channel).
    #[serde(default = "unlimited")]
    pub max_msgs_per_subject: i64,
    /// Largest accepted message in bytes.
    #[serde(default = "unlimited_i32")]
    pub max_msg_size: i32,
    /// Replicas (1 on a single server; 3 on a clustered hosted hub).
    #[serde(default = "one")]
    pub replicas: usize,
    #[serde(default = "file_storage")]
    pub storage: StorageKind,
}

fn unlimited() -> i64 {
    -1
}
fn unlimited_i32() -> i32 {
    -1
}
fn one() -> usize {
    1
}
fn file_storage() -> StorageKind {
    StorageKind::File
}

impl StreamLimits {
    fn new(name: &str, max_age_secs: u64, max_msgs_per_subject: i64) -> Self {
        Self {
            name: name.to_string(),
            max_age_secs,
            max_bytes: -1,
            max_msgs: -1,
            max_msgs_per_subject,
            max_msg_size: -1,
            replicas: 1,
            storage: StorageKind::File,
        }
    }

    pub fn max_age(&self) -> Duration {
        Duration::from_secs(self.max_age_secs)
    }
}

pub use nats_hub_client::delivery::config::{
    client_inbox_stream, client_tasks_stream, DEFAULT_INBOX_STREAM, DEFAULT_TASKS_STREAM,
};

/// Default ingress stream name.
pub const DEFAULT_INGRESS_STREAM: &str = "HUB_INGRESS";
/// The shared durable consumer every hub-server pulls ingress from.
pub const DEFAULT_ROUTER_CONSUMER: &str = "router";

const DAY: u64 = 24 * 60 * 60;

/// Durable delivery: three streams and the consumers on them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeliveryConfig {
    /// `hub.pub.*.>` + `hub.send.>`; work-queue, consumed by the router.
    pub ingress: StreamLimits,
    /// `channel.inbox.>`; one durable consumer per identity.
    pub inbox: StreamLimits,
    /// `channel.task.>`, `channel.wave.>`, `channel.session.>`; read by
    /// sequence for resume/catch-up.
    pub tasks: StreamLimits,
    /// Window in which a republish with the same `Nats-Msg-Id` is dropped.
    pub duplicate_window_secs: u64,
    /// Name of the shared ingress consumer.
    pub router_consumer: String,
    /// How long a pulled message may stay unacked before redelivery.
    pub ack_wait_secs: u64,
    /// Delivery attempts per ingress message before JetStream gives up
    /// (-1 = unlimited).
    pub router_max_deliver: i64,
    /// Unacked messages the router may hold (bounds work in flight).
    pub router_max_ack_pending: i64,
    /// Unacked DMs one identity's inbox consumer may hold.
    pub inbox_max_ack_pending: i64,
    /// An inbox consumer nobody has read for this long is removed (it is
    /// recreated on the next attach). 0 = never.
    pub inbox_inactive_secs: u64,
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            ingress: StreamLimits::new(DEFAULT_INGRESS_STREAM, DAY, -1),
            inbox: StreamLimits::new(DEFAULT_INBOX_STREAM, 7 * DAY, 10_000),
            tasks: StreamLimits::new(DEFAULT_TASKS_STREAM, DAY, 10_000),
            duplicate_window_secs: 120,
            router_consumer: DEFAULT_ROUTER_CONSUMER.to_string(),
            ack_wait_secs: 30,
            router_max_deliver: -1,
            router_max_ack_pending: 1_000,
            inbox_max_ack_pending: 256,
            inbox_inactive_secs: 7 * DAY,
        }
    }
}

impl DeliveryConfig {
    /// Load a JSON config file (any subset of the fields).
    pub fn from_file(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading delivery config {}", path.display()))?;
        let cfg: Self = serde_json::from_str(&text)
            .with_context(|| format!("parsing delivery config {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Reject names JetStream would refuse, and duplicate stream names.
    pub fn validate(&self) -> Result<()> {
        let names = [&self.ingress.name, &self.inbox.name, &self.tasks.name];
        for n in names.iter().chain([&&self.router_consumer]) {
            if !valid_name(n) {
                bail!("invalid stream/consumer name {n:?}: use [A-Za-z0-9_-]+");
            }
        }
        if names[0] == names[1] || names[0] == names[2] || names[1] == names[2] {
            bail!("ingress, inbox and tasks streams need distinct names");
        }
        Ok(())
    }

    /// Which stored stream (if any) captures a delivery subject. The router
    /// republishes those with a JetStream publish and waits for the store
    /// ack; other subjects (broadcast channels) stay core-NATS.
    pub fn stream_for(&self, dest: &str) -> Option<&str> {
        if dest.starts_with("channel.inbox.") {
            Some(&self.inbox.name)
        } else if TASKS_SUBJECTS
            .iter()
            .any(|s| dest.starts_with(s.trim_end_matches('>')))
        {
            Some(&self.tasks.name)
        } else {
            None
        }
    }
}

/// Subjects captured by the ingress stream.
pub const INGRESS_SUBJECTS: &[&str] = &["hub.pub.*.>", "hub.send.>"];
/// Subjects captured by the inbox stream.
pub const INBOX_SUBJECTS: &[&str] = &["channel.inbox.>"];
/// Subjects captured by the tasks stream.
pub const TASKS_SUBJECTS: &[&str] = &["channel.task.>", "channel.wave.>", "channel.session.>"];

/// A stream/consumer name JetStream accepts and that is a single NATS
/// token (it is embedded in `$JS.API.*` subjects and in ACLs).
pub fn valid_name(name: &str) -> bool {
    crate::protocol::valid_identity(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_classify_subjects() {
        let cfg = DeliveryConfig::default();
        cfg.validate().unwrap();
        assert_eq!(cfg.stream_for("channel.inbox.bob"), Some("HUB_INBOX"));
        assert_eq!(cfg.stream_for("channel.task.ab12"), Some("HUB_TASKS"));
        assert_eq!(cfg.stream_for("channel.wave.w1.task.t"), Some("HUB_TASKS"));
        assert_eq!(cfg.stream_for("channel.session.s1"), Some("HUB_TASKS"));
        assert_eq!(cfg.stream_for("channel.general"), None);
        assert_eq!(cfg.stream_for("channel.inboxes"), None);
    }

    #[test]
    fn partial_json_overrides_only_given_fields() {
        let cfg: DeliveryConfig = serde_json::from_str(
            r#"{"inbox": {"name": "T1_INBOX", "max_age_secs": 3600}, "ack_wait_secs": 5}"#,
        )
        .unwrap();
        assert_eq!(cfg.inbox.name, "T1_INBOX");
        assert_eq!(cfg.inbox.max_age_secs, 3600);
        assert_eq!(cfg.inbox.max_msgs_per_subject, -1); // field default
        assert_eq!(cfg.ack_wait_secs, 5);
        assert_eq!(cfg.tasks, DeliveryConfig::default().tasks);
    }

    #[test]
    fn rejects_bad_names_and_unknown_keys() {
        let mut cfg = DeliveryConfig::default();
        cfg.tasks.name = "HUB.TASKS".into();
        assert!(cfg.validate().is_err());
        cfg.tasks.name = cfg.inbox.name.clone();
        assert!(cfg.validate().is_err());
        assert!(serde_json::from_str::<DeliveryConfig>(r#"{"inbx": {}}"#).is_err());
    }
}
