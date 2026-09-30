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

use crate::protocol::Envelope;

mod types;
pub use types::*;

// ── Limits ───────────────────────────────────────────────────

/// Deepest reply level [`Storage::get_thread`] walks below the root.
pub const MAX_THREAD_DEPTH: usize = 64;

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

    /// Get a conversation thread starting from a root message: the root
    /// plus every reply below it (the full `reply_to` chain), walked up to
    /// [`MAX_THREAD_DEPTH`] levels.
    async fn get_thread(&self, root_id: &str) -> Result<Vec<EnvelopeRecord>>;

    /// Bounded [`get_thread`](Storage::get_thread): walk at most `max_depth`
    /// reply levels and return at most `limit` envelopes (root first).
    ///
    /// The default implementation truncates `get_thread`; backends should
    /// override it to bound the query itself.
    async fn get_thread_bounded(
        &self,
        root_id: &str,
        max_depth: usize,
        limit: usize,
    ) -> Result<Vec<EnvelopeRecord>> {
        let _ = max_depth;
        let mut thread = self.get_thread(root_id).await?;
        thread.truncate(limit);
        Ok(thread)
    }

    /// List pending (unanswered) messages for an agent: envelopes addressed
    /// to it (kind `message`/`human`) with no `kind = message` reply whose
    /// `reply_to` points at them. Newest first.
    async fn list_pending(&self, identity: &str) -> Result<Vec<EnvelopeRecord>>;

    /// Bounded [`list_pending`](Storage::list_pending): at most `limit` rows.
    ///
    /// The default implementation truncates `list_pending`; backends should
    /// override it to bound the query itself.
    async fn list_pending_bounded(
        &self,
        identity: &str,
        limit: usize,
    ) -> Result<Vec<EnvelopeRecord>> {
        let mut pending = self.list_pending(identity).await?;
        pending.truncate(limit);
        Ok(pending)
    }

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
mod dbtime;

#[cfg(feature = "storage-surreal")]
mod envelopes;

#[cfg(feature = "storage-surreal")]
pub mod schema;

#[cfg(feature = "storage-surreal")]
pub mod session;

#[cfg(feature = "storage-surreal")]
pub mod wave;

#[cfg(feature = "storage-surreal")]
pub mod surreal;

#[cfg(feature = "storage-surreal")]
pub use surreal::SurrealStorage;
