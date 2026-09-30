//! SurrealDB storage backend for nats-hub.
//!
//! Uses SurrealDB's embedded RocksDB engine for zero-config persistence.
//! Graph-native: conversation threading uses `->reply_to->` traversal.
//! Document-native: envelope payloads are stored as native JSON objects.
//!
//! SurrealDB is always behind the `Storage` trait — third parties never
//! get direct database access (BSL safeguard).
//!
//! The per-domain queries live in sibling modules (`agents`, `envelopes`,
//! `session`, `wave`, `schema`); this file owns the connection and the
//! `Storage` trait wiring.

use anyhow::{Context, Result};
use async_trait::async_trait;
use surrealdb::engine::local::RocksDb;
use surrealdb::Surreal;
use tracing::{debug, info};

use crate::protocol::Envelope;
use crate::storage::{agents, envelopes, schema, session, wave};
use crate::storage::{
    AgentFilter, AgentRecord, EnvelopeRecord, HistoryQuery, SessionFilter, SessionRecord, Storage,
    WaveRecord, WaveTaskRecord, MAX_THREAD_DEPTH,
};

/// Embedded SurrealDB client handle (what [`SurrealStorage::from_client`]
/// takes, and what every storage submodule queries through).
pub type Db = Surreal<surrealdb::engine::local::Db>;

/// SurrealDB-backed storage. Embedded RocksDB, zero-config.
/// In v2, `Surreal::new::<RocksDb>(path)` returns `Surreal<Db>`.
#[derive(Clone)]
pub struct SurrealStorage {
    db: Db,
}

impl SurrealStorage {
    /// Connect to an embedded SurrealDB instance at the given path.
    /// Creates the database file if it doesn't exist.
    pub async fn connect(path: &str) -> Result<Self> {
        debug!(%path, "connecting to SurrealDB (RocksDB embedded)");
        let db = Surreal::new::<RocksDb>(path)
            .await
            .context("failed to connect to SurrealDB RocksDB")?;

        db.use_ns("nats_hub").use_db("messaging").await?;

        info!(%path, "SurrealDB connected");
        Ok(Self { db })
    }

    /// Wrap an existing embedded SurrealDB client (selects the `nats_hub`
    /// namespace and `messaging` database on it). Useful for sharing one
    /// connection, and for tests that need to seed raw rows.
    pub async fn from_client(db: Db) -> Result<Self> {
        db.use_ns("nats_hub").use_db("messaging").await?;
        Ok(Self { db })
    }

    /// Connect to an in-memory SurrealDB (for testing).
    pub async fn connect_memory() -> Result<Self> {
        use surrealdb::engine::local::Mem;
        let db = Surreal::new::<Mem>(())
            .await
            .context("failed to connect to SurrealDB in-memory")?;

        db.use_ns("nats_hub").use_db("messaging").await?;
        Ok(Self { db })
    }
}

#[async_trait]
impl Storage for SurrealStorage {
    // ── Agent Registry ──────────────────────────────────────

    async fn register_agent(&self, agent: AgentRecord) -> Result<()> {
        agents::register_agent(&self.db, agent).await
    }

    async fn deregister_agent(&self, identity: &str) -> Result<()> {
        agents::deregister_agent(&self.db, identity).await
    }

    async fn touch_agent(&self, identity: &str) -> Result<()> {
        agents::touch_agent(&self.db, identity).await
    }

    async fn find_agents(&self, filter: &AgentFilter) -> Result<Vec<AgentRecord>> {
        agents::find_agents(&self.db, filter).await
    }

    async fn get_agent(&self, identity: &str) -> Result<Option<AgentRecord>> {
        agents::get_agent(&self.db, identity).await
    }

    // ── Message History ─────────────────────────────────────

    async fn store_envelope(&self, env: &Envelope) -> Result<()> {
        envelopes::store_envelope(&self.db, env).await
    }

    async fn query_history(&self, q: &HistoryQuery) -> Result<Vec<EnvelopeRecord>> {
        envelopes::query_history(&self.db, q).await
    }

    async fn get_envelope(&self, id: &str) -> Result<Option<EnvelopeRecord>> {
        envelopes::get_envelope(&self.db, id).await
    }

    // ── Conversation Threading (graph) ──────────────────────

    async fn link_reply(&self, reply_id: &str, parent_id: &str) -> Result<()> {
        envelopes::link_reply(&self.db, reply_id, parent_id).await
    }

    async fn get_thread(&self, root_id: &str) -> Result<Vec<EnvelopeRecord>> {
        envelopes::get_thread(&self.db, root_id, MAX_THREAD_DEPTH, None).await
    }

    async fn get_thread_bounded(
        &self,
        root_id: &str,
        max_depth: usize,
        limit: usize,
    ) -> Result<Vec<EnvelopeRecord>> {
        envelopes::get_thread(&self.db, root_id, max_depth, Some(limit)).await
    }

    async fn list_pending(&self, identity: &str) -> Result<Vec<EnvelopeRecord>> {
        envelopes::list_pending(&self.db, identity, None).await
    }

    async fn list_pending_bounded(
        &self,
        identity: &str,
        limit: usize,
    ) -> Result<Vec<EnvelopeRecord>> {
        envelopes::list_pending(&self.db, identity, Some(limit)).await
    }

    // ── Sessions ─────────────────────────────────────────────

    async fn create_session(&self, sess: SessionRecord) -> Result<()> {
        session::create_session(&self.db, sess).await
    }

    async fn update_session_status(&self, session_id: &str, status: &str) -> Result<()> {
        session::update_session_status(&self.db, session_id, status).await
    }

    async fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        session::get_session(&self.db, session_id).await
    }

    async fn list_sessions(&self, filter: &SessionFilter) -> Result<Vec<SessionRecord>> {
        session::list_sessions(&self.db, filter).await
    }

    // ── Waves ────────────────────────────────────────────────

    async fn create_wave(&self, wave: WaveRecord) -> Result<()> {
        wave::create_wave(&self.db, wave).await
    }

    async fn update_wave_status(&self, wave_id: &str, status: &str) -> Result<()> {
        wave::update_wave_status(&self.db, wave_id, status).await
    }

    async fn get_wave(&self, wave_id: &str) -> Result<Option<WaveRecord>> {
        wave::get_wave(&self.db, wave_id).await
    }

    async fn list_waves(&self, status: Option<&str>) -> Result<Vec<WaveRecord>> {
        wave::list_waves(&self.db, status).await
    }

    async fn create_wave_task(&self, task: WaveTaskRecord) -> Result<()> {
        wave::create_wave_task(&self.db, task).await
    }

    async fn update_wave_task_status(
        &self,
        wave_id: &str,
        task_id: &str,
        status: &str,
        result: Option<&str>,
    ) -> Result<()> {
        wave::update_wave_task_status(&self.db, wave_id, task_id, status, result).await
    }

    async fn get_wave_task(&self, wave_id: &str, task_id: &str) -> Result<Option<WaveTaskRecord>> {
        wave::get_wave_task(&self.db, wave_id, task_id).await
    }

    async fn list_wave_tasks(&self, wave_id: &str) -> Result<Vec<WaveTaskRecord>> {
        wave::list_wave_tasks(&self.db, wave_id).await
    }

    // ── Lifecycle ───────────────────────────────────────────

    async fn migrate(&self) -> Result<()> {
        schema::migrate(&self.db).await
    }

    async fn ping(&self) -> Result<()> {
        // Run a trivial query that returns no rows — just verify the DB
        // engine is responsive. Avoids SurrealDB v2.6 deserialization issues
        // with RecordId enum types in serde_json::Value.
        self.db.query("INFO FOR DB").await?;
        Ok(())
    }
}
