//! Storage trait + query types for nats-hub persistence.
//!
//! The `Storage` trait abstracts over database backends. The default
//! implementation is SurrealDB (graph-native, document-native, embedded).
//! Third parties never interact with the DB directly — they use the
//! nats-hub messaging API, and the `Storage` trait handles persistence
//! internally.
//!
//! See `docs/DATABASE_PLAN.md` for architecture details.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::protocol::Envelope;

// ── Query / Filter Types ─────────────────────────────────────

/// Filter for agent discovery queries.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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

// ── Storage Trait ────────────────────────────────────────────

/// Persistence backend for nats-hub.
///
/// Implementations:
/// - `SurrealStorage` (default, feature = "storage-surreal")
/// - Future: `PostgresStorage`, `LibsqlStorage`
///
/// The DB is a read/query sidecar — the hot path (NATS routing) never
/// blocks on Storage writes. Writes are async fire-and-forget off the
/// control plane router.
#[async_trait]
pub trait Storage: Send + Sync {
    // ── Agent Registry ──────────────────────────────────────

    /// Register or update an agent. Persisted across restarts.
    async fn register_agent(&self, agent: AgentRecord) -> Result<()>;

    /// Remove an agent from the registry.
    async fn deregister_agent(&self, identity: &str) -> Result<()>;

    /// Update agent liveness (called on heartbeat).
    async fn touch_agent(&self, identity: &str) -> Result<()>;

    /// Find agents matching a filter.
    async fn find_agents(&self, filter: &AgentFilter) -> Result<Vec<AgentRecord>>;

    /// Get a single agent by identity.
    async fn get_agent(&self, identity: &str) -> Result<Option<AgentRecord>>;

    // ── Message History ─────────────────────────────────────

    /// Store an envelope (called by the async mirror off the router).
    async fn store_envelope(&self, env: &Envelope) -> Result<()>;

    /// Query message history with filters.
    async fn query_history(&self, q: &HistoryQuery) -> Result<Vec<EnvelopeRecord>>;

    /// Get a single envelope by ID.
    async fn get_envelope(&self, id: &str) -> Result<Option<EnvelopeRecord>>;

    // ── Conversation Threading (graph) ──────────────────────

    /// Record a reply relationship (envelope A replies to envelope B).
    async fn link_reply(&self, reply_id: &str, parent_id: &str) -> Result<()>;

    /// Get a conversation thread starting from a root message.
    async fn get_thread(&self, root_id: &str) -> Result<Vec<EnvelopeRecord>>;

    /// List pending (unanswered) messages for an agent.
    async fn list_pending(&self, identity: &str) -> Result<Vec<EnvelopeRecord>>;

    // ── Sessions ─────────────────────────────────────────────

    /// Create a new session record.
    async fn create_session(&self, session: SessionRecord) -> Result<()>;

    /// Update a session's status (and optionally set closed_at when closing).
    async fn update_session_status(&self, session_id: &str, status: &str) -> Result<()>;

    /// Get a single session by ID.
    async fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>>;

    /// List sessions matching a filter.
    async fn list_sessions(&self, filter: &SessionFilter) -> Result<Vec<SessionRecord>>;

    // ── Waves ────────────────────────────────────────────────

    /// Create a new wave record.
    async fn create_wave(&self, wave: WaveRecord) -> Result<()>;

    /// Update a wave's status (sets closed_at when completed/failed).
    async fn update_wave_status(&self, wave_id: &str, status: &str) -> Result<()>;

    /// Get a single wave by ID.
    async fn get_wave(&self, wave_id: &str) -> Result<Option<WaveRecord>>;

    /// List waves, optionally filtered by status.
    async fn list_waves(&self, status: Option<&str>) -> Result<Vec<WaveRecord>>;

    /// Create a wave task record.
    async fn create_wave_task(&self, task: WaveTaskRecord) -> Result<()>;

    /// Update a wave task's status and optional result.
    async fn update_wave_task_status(
        &self,
        wave_id: &str,
        task_id: &str,
        status: &str,
        result: Option<&str>,
    ) -> Result<()>;

    /// Get a single wave task.
    async fn get_wave_task(&self, wave_id: &str, task_id: &str) -> Result<Option<WaveTaskRecord>>;

    /// List all tasks for a wave.
    async fn list_wave_tasks(&self, wave_id: &str) -> Result<Vec<WaveTaskRecord>>;

    // ── Lifecycle ───────────────────────────────────────────

    /// Initialize the schema (create tables, indexes, etc.).
    async fn migrate(&self) -> Result<()>;

    /// Health check.
    async fn ping(&self) -> Result<()>;
}

// ── Module wiring ────────────────────────────────────────────

#[cfg(feature = "storage-surreal")]
mod agents;

#[cfg(feature = "storage-surreal")]
mod envelopes;

#[cfg(feature = "storage-surreal")]
mod schema;

#[cfg(feature = "storage-surreal")]
pub mod session;

#[cfg(feature = "storage-surreal")]
pub mod wave;

#[cfg(feature = "storage-surreal")]
pub mod surreal;

#[cfg(feature = "storage-surreal")]
pub use surreal::SurrealStorage;
