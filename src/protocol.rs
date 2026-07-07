//! Wire protocol for nats-hub.
//!
//! Every message on the bus is a JSON-serialised [`Envelope`].
//! Agents publish to `hub.send.<channel>` and the control plane
//! routes them to `channel.<name>` for subscribers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What kind of payload is inside the envelope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    /// Agent → agent or agent → channel communication.
    Message,
    /// A control / system message (registration, presence, route updates).
    Control,
    /// A human-authored message injected via `hub-interact`.
    Human,
    /// An agent status update (e.g. "thinking", "idle", "working").
    Status,
    /// A typed progress event during worker execution (see `event_type` in payload).
    Event,
}

/// Metadata attached to every envelope — identifies sender, intended
/// channel, and routing hints for the control plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    /// Globally-unique message ID (auto-generated if absent).
    pub id: String,
    /// Sender identity (any string — agent name, human name, "system").
    pub from: String,
    /// Intended channel / topic (e.g. "agents.worker1", "humans.observers").
    pub channel: String,
    /// Optional direct recipient (bypasses broadcast on the channel).
    pub to: Option<String>,
    /// Timestamp (UTC, RFC 3339).
    pub timestamp: DateTime<Utc>,
    /// Kind of message.
    pub kind: MessageKind,
    /// Optional correlation ID for request/reply patterns.
    pub reply_to: Option<String>,
}

/// The unit of communication on the bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub meta: Meta,
    /// Free-form JSON payload — agents decide their own schemas per channel.
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl Envelope {
    /// Create a new envelope with auto-generated ID and current timestamp.
    pub fn new(
        from: impl Into<String>,
        channel: impl Into<String>,
        kind: MessageKind,
        payload: serde_json::Value,
    ) -> Self {
        Self {
            meta: Meta {
                id: Uuid::new_v4().to_string(),
                from: from.into(),
                channel: channel.into(),
                to: None,
                timestamp: Utc::now(),
                kind,
                reply_to: None,
            },
            payload,
        }
    }

    /// Builder: set direct recipient.
    pub fn to(mut self, recipient: impl Into<String>) -> Self {
        self.meta.to = Some(recipient.into());
        self
    }

    /// Builder: set correlation ID.
    pub fn reply_to(mut self, id: impl Into<String>) -> Self {
        self.meta.reply_to = Some(id.into());
        self
    }

    /// Serialise to JSON bytes.
    pub fn to_json_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }

    /// Deserialise from JSON bytes.
    pub fn from_json_bytes(data: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(data)?)
    }
}

/// Subject conventions used through nats-hub.
pub mod subjects {
    /// Agents publish messages here: `hub.send.<channel>`.
    /// The control plane subscribes to `hub.send.>` and routes.
    pub const SEND_PREFIX: &str = "hub.send";

    /// The control plane publishes routed messages here: `channel.<name>`.
    pub const CHANNEL_PREFIX: &str = "channel";

    /// Registration subject for agents joining the bus.
    pub const REGISTER: &str = "hub.register";

    /// Presence / heartbeat subject.
    pub const PRESENCE: &str = "hub.presence";

    /// Build a send subject: `hub.send.<channel>`.
    pub fn send(channel: &str) -> String {
        format!("{SEND_PREFIX}.{channel}")
    }

    /// Build a channel subject: `channel.<name>`.
    pub fn channel(name: &str) -> String {
        format!("{CHANNEL_PREFIX}.{name}")
    }

    /// Build an inbox subject for direct messaging: `channel.inbox.<identity>`.
    /// Used when `Envelope.meta.to` is set — the router routes to this subject
    /// instead of the broadcast channel.
    pub fn inbox(identity: &str) -> String {
        format!("{CHANNEL_PREFIX}.inbox.{identity}")
    }

    /// Wave-level broadcast channel: `channel.wave.<wave-id>`.
    pub fn wave(wave_id: &str) -> String {
        format!("{CHANNEL_PREFIX}.wave.{wave_id}")
    }

    /// Per-task channel within a wave: `channel.wave.<wave-id>.task.<task-id>`.
    pub fn wave_task(wave_id: &str, task_id: &str) -> String {
        format!("{CHANNEL_PREFIX}.wave.{wave_id}.task.{task_id}")
    }

    /// Hub send channel name for a wave (no `channel.` prefix).
    pub fn wave_channel_name(wave_id: &str) -> String {
        format!("wave.{wave_id}")
    }

    /// Hub send channel name for a wave task.
    pub fn wave_task_channel_name(wave_id: &str, task_id: &str) -> String {
        format!("wave.{wave_id}.task.{task_id}")
    }
}
