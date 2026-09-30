//! Query, filter and record types used by the `Storage` trait.
//!
//! Split from `storage/mod.rs` to keep files under 400 LOC; re-exported
//! from `crate::storage`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::protocol::Envelope;

// ── Query / Filter Types ─────────────────────────────────────

/// Filter for agent discovery queries.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AgentFilter {
    /// Only return agents with ALL these capabilities.
    pub capabilities: Vec<String>,
    /// Only return agents seen within this many seconds.
    pub alive_within_secs: Option<i64>,
    /// Limit number of results.
    pub limit: Option<usize>,
}

impl AgentFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn capabilities(mut self, caps: Vec<String>) -> Self {
        self.capabilities = caps;
        self
    }

    pub fn alive_within(mut self, secs: i64) -> Self {
        self.alive_within_secs = Some(secs);
        self
    }

    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }
}

/// Query for message history.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HistoryQuery {
    /// Filter by channel (exact match).
    pub channel: Option<String>,
    /// Filter by sender identity.
    pub from: Option<String>,
    /// Filter by direct recipient identity (DM target).
    pub to: Option<String>,
    /// Filter by message kind.
    pub kind: Option<String>,
    /// Only messages after this timestamp.
    pub since: Option<DateTime<Utc>>,
    /// Only messages before this timestamp.
    pub until: Option<DateTime<Utc>>,
    /// Limit number of results (most recent first).
    pub limit: Option<usize>,
}

impl HistoryQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn channel(mut self, ch: impl Into<String>) -> Self {
        self.channel = Some(ch.into());
        self
    }

    pub fn from(mut self, id: impl Into<String>) -> Self {
        self.from = Some(id.into());
        self
    }

    /// Filter by direct recipient (DM target).
    pub fn to(mut self, id: impl Into<String>) -> Self {
        self.to = Some(id.into());
        self
    }

    pub fn kind(mut self, k: impl Into<String>) -> Self {
        self.kind = Some(k.into());
        self
    }

    pub fn since(mut self, ts: DateTime<Utc>) -> Self {
        self.since = Some(ts);
        self
    }

    pub fn until(mut self, ts: DateTime<Utc>) -> Self {
        self.until = Some(ts);
        self
    }

    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }
}

// ── Record Types ─────────────────────────────────────────────

/// A persisted agent record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub identity: String,
    pub capabilities: Vec<String>,
    pub last_seen: DateTime<Utc>,
    pub registered_at: DateTime<Utc>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// A persisted envelope record (stored copy of an Envelope).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopeRecord {
    pub id: String,
    pub from_identity: String,
    pub channel: String,
    pub to_identity: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub kind: String,
    pub reply_to: Option<String>,
    pub payload: serde_json::Value,
    pub stored_at: DateTime<Utc>,
}

impl EnvelopeRecord {
    /// Convert a live Envelope into a record for storage.
    pub fn from_envelope(env: &Envelope) -> Self {
        Self {
            id: env.meta.id.clone(),
            from_identity: env.meta.from.clone(),
            channel: env.meta.channel.clone(),
            to_identity: env.meta.to.clone(),
            timestamp: env.meta.timestamp,
            kind: format!("{:?}", env.meta.kind).to_lowercase(),
            reply_to: env.meta.reply_to.clone(),
            payload: env.payload.clone(),
            stored_at: Utc::now(),
        }
    }
}

// ── Session Types ────────────────────────────────────────────

/// A persisted session record — a multi-turn conversation between
/// an orchestrator and a worker on `channel.session.<uuid>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub orchestrator: String,
    pub worker: String,
    pub status: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Filter for session queries.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SessionFilter {
    pub status: Option<String>,
    pub worker: Option<String>,
    pub orchestrator: Option<String>,
    pub limit: Option<usize>,
}

impl SessionFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn status(mut self, s: impl Into<String>) -> Self {
        self.status = Some(s.into());
        self
    }

    pub fn worker(mut self, w: impl Into<String>) -> Self {
        self.worker = Some(w.into());
        self
    }

    pub fn orchestrator(mut self, o: impl Into<String>) -> Self {
        self.orchestrator = Some(o.into());
        self
    }

    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }
}

// ── Wave Types ───────────────────────────────────────────────

/// A persisted wave record — parallel tasks with disjoint write scopes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveRecord {
    pub wave_id: String,
    pub goal: String,
    pub status: String,
    pub orchestrator: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// A single task within a wave.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveTaskRecord {
    pub wave_id: String,
    pub task_id: String,
    pub worker: String,
    pub goal: String,
    pub status: String,
    #[serde(default)]
    pub write_scope: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub handoff_path: Option<String>,
    #[serde(default)]
    pub verify_cmd: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub result: Option<String>,
}
