//! SurrealDB storage backend for nats-hub.
//!
//! Uses SurrealDB's embedded RocksDB engine for zero-config persistence.
//! Graph-native: conversation threading uses `->reply_to->` traversal.
//! Document-native: envelope payloads are stored as native JSON objects.
//!
//! SurrealDB is always behind the `Storage` trait — third parties never
//! get direct database access (BSL safeguard).

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::engine::local::RocksDb;
use surrealdb::RecordId;
use surrealdb::Surreal;
use tracing::{debug, info, warn};

use crate::protocol::Envelope;
use crate::storage::session;
use crate::storage::{
    AgentFilter, AgentRecord, EnvelopeRecord, HistoryQuery, SessionFilter, SessionRecord, Storage,
};

/// SurrealDB-backed storage. Embedded RocksDB, zero-config.
/// In v2, `Surreal::new::<RocksDb>(path)` returns `Surreal<Db>`.
pub struct SurrealStorage {
    db: Surreal<surrealdb::engine::local::Db>,
}

/// Internal row type for agents table.
/// `id` is the SurrealDB record ID (e.g. `agents:agent-gamma`).
#[derive(Debug, Serialize, Deserialize)]
struct AgentRow {
    #[serde(default)]
    id: Option<RecordId>,
    #[serde(default)]
    identity: String,
    capabilities: Vec<String>,
    last_seen: DateTime<Utc>,
    registered_at: DateTime<Utc>,
    #[serde(default)]
    metadata: serde_json::Value,
}

/// Internal row type for envelopes table (without ID — ID is the record key).
#[derive(Debug, Serialize, Deserialize)]
struct EnvelopeRow {
    from_identity: String,
    channel: String,
    to_identity: Option<String>,
    timestamp: DateTime<Utc>,
    kind: String,
    reply_to: Option<String>,
    payload: serde_json::Value,
    stored_at: DateTime<Utc>,
}

/// Row type that includes the record ID (for queries that return it).
#[derive(Debug, Serialize, Deserialize)]
struct EnvelopeRowWithId {
    id: RecordId,
    from_identity: String,
    channel: String,
    to_identity: Option<String>,
    timestamp: DateTime<Utc>,
    kind: String,
    reply_to: Option<String>,
    payload: serde_json::Value,
    stored_at: DateTime<Utc>,
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
        debug!(identity = %agent.identity, "storing agent record");

        let row = AgentRow {
            id: None,
            identity: agent.identity.clone(), // sent as content but ignored on store
            capabilities: agent.capabilities,
            last_seen: agent.last_seen,
            registered_at: agent.registered_at,
            metadata: agent.metadata,
        };

        let _: Option<AgentRow> = self
            .db
            .upsert(("agents", &agent.identity))
            .content(row)
            .await
            .context("failed to upsert agent")?;

        Ok(())
    }

    async fn deregister_agent(&self, identity: &str) -> Result<()> {
        debug!(%identity, "deregistering agent");

        let _: Option<AgentRow> = self
            .db
            .delete(("agents", identity))
            .await
            .context("failed to delete agent")?;

        Ok(())
    }

    async fn touch_agent(&self, identity: &str) -> Result<()> {
        debug!(%identity, "touching agent liveness");

        let now = Utc::now();
        let _: Option<AgentRow> = self
            .db
            .query("UPDATE type::thing('agents', $id) SET last_seen = $now")
            .bind(("id", identity.to_string()))
            .bind(("now", now))
            .await?
            .take(0)?;

        Ok(())
    }

    async fn find_agents(&self, filter: &AgentFilter) -> Result<Vec<AgentRecord>> {
        debug!(?filter, "finding agents");

        let mut query = String::from("SELECT id, * FROM agents");
        let mut conditions: Vec<String> = vec![];

        if let Some(_secs) = filter.alive_within_secs {
            conditions.push("last_seen > $cutoff".to_string());
        }

        if !conditions.is_empty() {
            query.push_str(" WHERE ");
            query.push_str(&conditions.join(" AND "));
        }

        if let Some(limit) = filter.limit {
            query.push_str(&format!(" LIMIT {limit}"));
        }

        let mut q_builder = self.db.query(query);

        if let Some(secs) = filter.alive_within_secs {
            let cutoff = Utc::now() - chrono::Duration::seconds(secs);
            q_builder = q_builder.bind(("cutoff", cutoff));
        }

        let rows: Vec<AgentRow> = q_builder.await?.take(0)?;

        let agents: Vec<AgentRecord> = rows
            .into_iter()
            .filter(|row| {
                if filter.capabilities.is_empty() {
                    return true;
                }
                filter
                    .capabilities
                    .iter()
                    .all(|cap| row.capabilities.contains(cap))
            })
            .map(|row| {
                let ident = row.id
                    .as_ref()
                    .map(|id| {
                        let s = id.key().to_string();
                        // Strip SurrealDB backtick quoting from string keys
                        s.trim_matches('`').to_string()
                    })
                    .unwrap_or_default();
                AgentRecord {
                    identity: ident,
                    capabilities: row.capabilities,
                    last_seen: row.last_seen,
                    registered_at: row.registered_at,
                    metadata: row.metadata,
                }
            })
            .collect();

        Ok(agents)
    }

    async fn get_agent(&self, identity: &str) -> Result<Option<AgentRecord>> {
        debug!(%identity, "getting agent");

        let mut result = self
            .db
            .query("SELECT * FROM type::thing('agents', $id)")
            .bind(("id", identity.to_string()))
            .await?;

        let rows: Vec<AgentRow> = result.take(0)?;

        Ok(rows.into_iter().next().map(|r| {
            let ident = r.id
                .as_ref()
                .map(|id| {
                    let s = id.key().to_string();
                    s.trim_matches('`').to_string()
                })
                .unwrap_or_default();
            AgentRecord {
                identity: ident,
                capabilities: r.capabilities,
                last_seen: r.last_seen,
                registered_at: r.registered_at,
                metadata: r.metadata,
            }
        }))
    }

    // ── Message History ─────────────────────────────────────

    async fn store_envelope(&self, env: &Envelope) -> Result<()> {
        debug!(id = %env.meta.id, "storing envelope");

        let record = EnvelopeRecord::from_envelope(env);
        let row = EnvelopeRow {
            from_identity: record.from_identity,
            channel: record.channel,
            to_identity: record.to_identity,
            timestamp: record.timestamp,
            kind: record.kind,
            reply_to: record.reply_to,
            payload: record.payload,
            stored_at: record.stored_at,
        };

        let _: Option<EnvelopeRow> = self
            .db
            .create(("envelopes", &record.id))
            .content(row)
            .await
            .context("failed to store envelope")?;

        // If this envelope is a reply, create the graph edge
        if let Some(parent_id) = &env.meta.reply_to {
            if let Err(e) = self.link_reply(&env.meta.id, parent_id).await {
                warn!(error = %e, "failed to link reply edge (non-fatal)");
            }
        }

        Ok(())
    }

    async fn query_history(&self, q: &HistoryQuery) -> Result<Vec<EnvelopeRecord>> {
        debug!(?q, "querying history");

        let mut query = String::from("SELECT * FROM envelopes");
        let mut conditions: Vec<String> = vec![];

        if q.channel.is_some() {
            conditions.push("channel = $channel".to_string());
        }
        if q.from.is_some() {
            conditions.push("from_identity = $from".to_string());
        }
        if q.kind.is_some() {
            conditions.push("kind = $kind".to_string());
        }
        if q.since.is_some() {
            conditions.push("timestamp > $since".to_string());
        }
        if q.until.is_some() {
            conditions.push("timestamp < $until".to_string());
        }

        if !conditions.is_empty() {
            query.push_str(" WHERE ");
            query.push_str(&conditions.join(" AND "));
        }

        query.push_str(" ORDER BY timestamp DESC");

        if let Some(limit) = q.limit {
            query.push_str(&format!(" LIMIT {limit}"));
        }

        let mut q_builder = self.db.query(query);

        if let Some(ref ch) = q.channel {
            q_builder = q_builder.bind(("channel", ch.clone()));
        }
        if let Some(ref from) = q.from {
            q_builder = q_builder.bind(("from", from.clone()));
        }
        if let Some(ref kind) = q.kind {
            q_builder = q_builder.bind(("kind", kind.clone()));
        }
        if let Some(since) = q.since {
            q_builder = q_builder.bind(("since", since));
        }
        if let Some(until) = q.until {
            q_builder = q_builder.bind(("until", until));
        }

        let rows: Vec<EnvelopeRowWithId> = q_builder.await?.take(0)?;

        Ok(rows
            .into_iter()
            .map(|r| EnvelopeRecord {
                id: r.id.to_string(),
                from_identity: r.from_identity,
                channel: r.channel,
                to_identity: r.to_identity,
                timestamp: r.timestamp,
                kind: r.kind,
                reply_to: r.reply_to,
                payload: r.payload,
                stored_at: r.stored_at,
            })
            .collect())
    }

    async fn get_envelope(&self, id: &str) -> Result<Option<EnvelopeRecord>> {
        debug!(%id, "getting envelope");

        let mut result = self
            .db
            .query("SELECT id, from_identity, channel, to_identity, timestamp, kind, reply_to, payload, stored_at FROM type::thing('envelopes', $id)")
            .bind(("id", id.to_string()))
            .await?;

        let rows: Vec<EnvelopeRowWithId> = result.take(0)?;

        Ok(rows.into_iter().next().map(|r| EnvelopeRecord {
            id: r.id.to_string(),
            from_identity: r.from_identity,
            channel: r.channel,
            to_identity: r.to_identity,
            timestamp: r.timestamp,
            kind: r.kind,
            reply_to: r.reply_to,
            payload: r.payload,
            stored_at: r.stored_at,
        }))
    }

    // ── Conversation Threading (graph) ──────────────────────

    async fn link_reply(&self, reply_id: &str, parent_id: &str) -> Result<()> {
        debug!(%reply_id, %parent_id, "linking reply edge");

        let _: Option<serde_json::Value> = self
            .db
            .query("RELATE type::thing('envelopes', $reply)->reply_to->type::thing('envelopes', $parent)")
            .bind(("reply", reply_id.to_string()))
            .bind(("parent", parent_id.to_string()))
            .await?
            .take(0)?;

        Ok(())
    }

    async fn get_thread(&self, root_id: &str) -> Result<Vec<EnvelopeRecord>> {
        debug!(%root_id, "fetching thread (graph traversal)");

        // Get root + all envelopes that have reply_to = root_id
        // (Simpler than graph traversal, works reliably in v2)
        let mut result = self
            .db
            .query(
                "SELECT id, from_identity, channel, to_identity, timestamp, kind, reply_to, payload, stored_at \
                 FROM type::thing('envelopes', $root); \
                 SELECT id, from_identity, channel, to_identity, timestamp, kind, reply_to, payload, stored_at \
                 FROM envelopes WHERE reply_to = $root"
            )
            .bind(("root", root_id.to_string()))
            .await?;

        let root_rows: Vec<EnvelopeRowWithId> = result.take(0)?;
        let reply_rows: Vec<EnvelopeRowWithId> = result.take(1)?;

        let mut rows = root_rows;
        rows.extend(reply_rows);

        Ok(rows
            .into_iter()
            .map(|r| EnvelopeRecord {
                id: r.id.to_string(),
                from_identity: r.from_identity,
                channel: r.channel,
                to_identity: r.to_identity,
                timestamp: r.timestamp,
                kind: r.kind,
                reply_to: r.reply_to,
                payload: r.payload,
                stored_at: r.stored_at,
            })
            .collect())
    }

    async fn list_pending(&self, identity: &str) -> Result<Vec<EnvelopeRecord>> {
        debug!(%identity, "listing pending messages");

        let mut result = self
            .db
            .query(
                "SELECT id, from_identity, channel, to_identity, timestamp, kind, reply_to, payload, stored_at \
                 FROM envelopes \
                 WHERE to_identity = $identity \
                 AND id NOT IN (SELECT ->reply_to->envelopes.id FROM envelopes WHERE to_identity = $identity)"
            )
            .bind(("identity", identity.to_string()))
            .await?;

        let rows: Vec<EnvelopeRowWithId> = result.take(0)?;

        Ok(rows
            .into_iter()
            .map(|r| EnvelopeRecord {
                id: r.id.to_string(),
                from_identity: r.from_identity,
                channel: r.channel,
                to_identity: r.to_identity,
                timestamp: r.timestamp,
                kind: r.kind,
                reply_to: r.reply_to,
                payload: r.payload,
                stored_at: r.stored_at,
            })
            .collect())
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

    // ── Lifecycle ───────────────────────────────────────────

    async fn migrate(&self) -> Result<()> {
        info!("running SurrealDB schema migration");

        let queries = [
            "DEFINE TABLE agents SCHEMALESS",
            "DEFINE FIELD identity       AT agents TYPE string",
            "DEFINE FIELD capabilities   AT agents TYPE array<string>",
            "DEFINE FIELD last_seen      AT agents TYPE datetime",
            "DEFINE FIELD registered_at  AT agents TYPE datetime",
            "DEFINE FIELD metadata       AT agents TYPE object",
            "DEFINE INDEX idx_agents_last_seen ON TABLE agents COLUMNS last_seen",
            "DEFINE TABLE envelopes SCHEMALESS",
            "DEFINE FIELD from_identity AT envelopes TYPE string",
            "DEFINE FIELD channel       AT envelopes TYPE string",
            "DEFINE FIELD to_identity   AT envelopes TYPE option<string>",
            "DEFINE FIELD timestamp     AT envelopes TYPE datetime",
            "DEFINE FIELD kind          AT envelopes TYPE string",
            "DEFINE FIELD reply_to      AT envelopes TYPE option<string>",
            "DEFINE FIELD payload       AT envelopes TYPE object",
            "DEFINE FIELD stored_at     AT envelopes TYPE datetime",
            "DEFINE INDEX idx_env_channel_time ON TABLE envelopes COLUMNS channel, timestamp",
            "DEFINE INDEX idx_env_from_time    ON TABLE envelopes COLUMNS from_identity, timestamp",
            "DEFINE INDEX idx_env_kind_time    ON TABLE envelopes COLUMNS kind, timestamp",
            "DEFINE TABLE reply_to SCHEMALESS TYPE RELATION FROM envelopes TO envelopes",
            // Sessions table
            "DEFINE TABLE sessions SCHEMALESS",
            "DEFINE FIELD session_id    AT sessions TYPE string",
            "DEFINE FIELD orchestrator  AT sessions TYPE string",
            "DEFINE FIELD worker        AT sessions TYPE string",
            "DEFINE FIELD status        AT sessions TYPE string",
            "DEFINE FIELD cwd           AT sessions TYPE option<string>",
            "DEFINE FIELD model         AT sessions TYPE option<string>",
            "DEFINE FIELD provider      AT sessions TYPE option<string>",
            "DEFINE FIELD created_at    AT sessions TYPE datetime",
            "DEFINE FIELD updated_at    AT sessions TYPE datetime",
            "DEFINE FIELD closed_at     AT sessions TYPE option<datetime>",
            "DEFINE FIELD metadata      AT sessions TYPE object",
            "DEFINE INDEX idx_sessions_status ON TABLE sessions COLUMNS status",
            "DEFINE INDEX idx_sessions_worker ON TABLE sessions COLUMNS worker, status",
        ];

        for q in &queries {
            if let Err(e) = self.db.query(*q).await {
                debug!(error = %e, "migration statement (non-fatal): {q}");
            }
        }

        info!("SurrealDB schema migration complete");
        Ok(())
    }

    async fn ping(&self) -> Result<()> {
        let mut result = self.db.query("SELECT * FROM type::table('agents') LIMIT 1").await?;
        let _: Vec<serde_json::Value> = result.take(0)?;
        Ok(())
    }
}
