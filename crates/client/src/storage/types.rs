//! Query, filter and record types used by the `Storage` trait.
//!
//! Split from `storage/mod.rs` to keep files under 400 LOC; re-exported
//! from `crate::storage`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::protocol::{Envelope, Origin};

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
    /// Filter by owning user (`origin.user`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Filter by agent client (`origin.agent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Filter by self-reported model (`origin.reported.model`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Filter by self-reported session (`origin.reported.session`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
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

    /// Filter by owning user (`origin.user`).
    pub fn user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    /// Filter by agent client (`origin.agent`).
    pub fn agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Filter by self-reported model (`origin.reported.model`).
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Filter by self-reported session (`origin.reported.session`).
    pub fn session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self
    }

    /// Names of the filters this query sets. `history.query` echoes them, so
    /// a client can tell when an older hub ignored a filter it doesn't know.
    pub fn applied_filters(&self) -> Vec<&'static str> {
        [
            ("channel", self.channel.is_some()),
            ("from", self.from.is_some()),
            ("to", self.to.is_some()),
            ("kind", self.kind.is_some()),
            ("since", self.since.is_some()),
            ("until", self.until.is_some()),
            ("user", self.user.is_some()),
            ("agent", self.agent.is_some()),
            ("model", self.model.is_some()),
            ("session", self.session.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect()
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
    /// Provenance as stamped by the router when the envelope was routed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
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
            origin: env.meta.origin.clone(),
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
    /// Backend-native session reference (e.g. `{"claude_session_id": "…"}`,
    /// `{"thread_id": "…"}`, `{"agent_id": "…"}`) so a restarted worker can
    /// resume a session it no longer holds in memory. Owned by the worker;
    /// written via the `session.set_backend_ctx` API op.
    #[serde(default)]
    pub backend_ctx: Option<serde_json::Value>,
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

// ── File-lock Types ─────────────────────────────────────────

/// A persisted file lock — an advisory "I'm working on these paths"
/// announcement shared by agents editing the same repo (see
/// `docs/FILE_LOCKS.md`). Held until released, until `expires_at`, or until
/// the holder's presence is lost (the hub's liveness TTL).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockRecord {
    pub lock_id: String,
    /// Coordination namespace: a repo or project key (free string, e.g. a
    /// git remote or repo path). Locks only conflict within one scope.
    pub scope: String,
    /// Repo-relative files, or directory prefixes ending in `/`.
    pub paths: Vec<String>,
    /// Lock owner — stamped from the caller's bound identity by the hub,
    /// never trusted from the payload.
    pub holder: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    pub ttl_secs: u64,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
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
    /// Outcome of the task's `verify_cmd`, recorded by the wave orchestrator
    /// from the worker's `milestone verify_passed` event (`"passed"`), a
    /// worker `error` event (`"failed"`), or a task completing without the
    /// expected milestone (`"missing"`). `None` when the task has no
    /// `verify_cmd` or hasn't finished.
    #[serde(default)]
    pub verify_result: Option<String>,
}
