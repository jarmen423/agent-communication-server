//! Session storage methods for SurrealDB.
//!
//! Split from `surreal.rs` to keep files under 400 LOC.
//! Implements the session-related methods of the `Storage` trait
//! on `SurrealStorage`.
//!
//! Datetimes are stored as native SurrealDB datetimes and `metadata` is
//! persisted. Reads tolerate legacy rows (RFC 3339 strings, `""` for unset
//! optionals); a corrupt required datetime is reported, never replaced.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::storage::dbtime::{db_now, db_time, metadata_object, DbTime, StoredTime};
use crate::storage::surreal::Db;
use crate::storage::{SessionFilter, SessionRecord};

/// Internal row type written to the sessions table. No `id` field —
/// SurrealDB assigns the record ID from the key passed to `.upsert()`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionRow {
    pub session_id: String,
    pub orchestrator: String,
    pub worker: String,
    pub status: String,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub created_at: DbTime,
    pub updated_at: DbTime,
    pub closed_at: Option<DbTime>,
    pub metadata: serde_json::Value,
    pub backend_ctx: Option<serde_json::Value>,
}

/// Row type read back from the DB (includes the record ID; tolerant of
/// legacy string datetimes and missing metadata).
#[derive(Debug, Deserialize)]
pub struct SessionRowWithId {
    pub id: surrealdb::RecordId,
    pub session_id: String,
    pub orchestrator: String,
    pub worker: String,
    pub status: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    created_at: StoredTime,
    updated_at: StoredTime,
    #[serde(default)]
    closed_at: Option<StoredTime>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
    #[serde(default)]
    backend_ctx: Option<serde_json::Value>,
}

/// Legacy rows used `""` for "unset".
fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.is_empty())
}

impl SessionRowWithId {
    /// Convert a DB row into a public `SessionRecord`. Errors if a required
    /// datetime is corrupt.
    pub fn to_record(self) -> Result<SessionRecord> {
        let record = self.id.to_string();
        Ok(SessionRecord {
            created_at: self.created_at.required(&record, "created_at")?,
            updated_at: self.updated_at.required(&record, "updated_at")?,
            closed_at: StoredTime::optional(self.closed_at, &record, "closed_at"),
            session_id: self.session_id,
            orchestrator: self.orchestrator,
            worker: self.worker,
            status: self.status,
            cwd: non_empty(self.cwd),
            model: non_empty(self.model),
            provider: non_empty(self.provider),
            metadata: metadata_object(self.metadata.unwrap_or_default()),
            backend_ctx: self.backend_ctx.filter(|v| !v.is_null()),
        })
    }
}

/// SQL columns shared between SELECT queries and row conversion.
const SESSION_COLUMNS: &str = "id, session_id, orchestrator, worker, status, \
     cwd, model, provider, created_at, updated_at, closed_at, metadata, backend_ctx";

/// Convert a `SessionRecord` into a `SessionRow` for storage.
pub fn session_to_row(record: &SessionRecord) -> SessionRow {
    SessionRow {
        session_id: record.session_id.clone(),
        orchestrator: record.orchestrator.clone(),
        worker: record.worker.clone(),
        status: record.status.clone(),
        cwd: record.cwd.clone(),
        model: record.model.clone(),
        provider: record.provider.clone(),
        created_at: db_time(record.created_at),
        updated_at: db_time(record.updated_at),
        closed_at: record.closed_at.map(db_time),
        metadata: metadata_object(record.metadata.clone()),
        backend_ctx: record.backend_ctx.clone(),
    }
}

/// Create a session in the DB.
pub async fn create_session(db: &Db, session: SessionRecord) -> Result<()> {
    debug!(session_id = %session.session_id, "storing session record");
    let row = session_to_row(&session);
    let _: Option<SessionRow> = db
        .upsert(("sessions", &session.session_id))
        .content(row)
        .await
        .context("failed to create session")?;
    Ok(())
}

/// Update a session's status. Sets `closed_at` when status is "closed".
pub async fn update_session_status(db: &Db, session_id: &str, status: &str) -> Result<()> {
    debug!(%session_id, %status, "updating session status");
    let sql = if status == "closed" {
        "UPDATE type::thing('sessions', $id) SET status = $status, updated_at = $now, closed_at = $now"
    } else {
        "UPDATE type::thing('sessions', $id) SET status = $status, updated_at = $now"
    };
    db.query(sql)
        .bind(("id", session_id.to_string()))
        .bind(("status", status.to_string()))
        .bind(("now", db_now()))
        .await?
        .check()
        .with_context(|| format!("failed to update session '{session_id}'"))?;
    Ok(())
}

/// Get a single session by ID.
pub async fn get_session(db: &Db, session_id: &str) -> Result<Option<SessionRecord>> {
    debug!(%session_id, "getting session");
    let rows: Vec<SessionRowWithId> = db
        .query(format!(
            "SELECT {SESSION_COLUMNS} FROM type::thing('sessions', $id)"
        ))
        .bind(("id", session_id.to_string()))
        .await?
        .check()?
        .take(0)?;
    rows.into_iter().next().map(|r| r.to_record()).transpose()
}

/// List sessions matching a filter. Rows with a corrupt required datetime
/// are skipped (and logged) rather than failing the whole list.
pub async fn list_sessions(db: &Db, filter: &SessionFilter) -> Result<Vec<SessionRecord>> {
    debug!(?filter, "listing sessions");

    let mut query = format!("SELECT {SESSION_COLUMNS} FROM sessions");
    let mut conditions: Vec<String> = vec![];

    if filter.status.is_some() {
        conditions.push("status = $status".to_string());
    }
    if filter.worker.is_some() {
        conditions.push("worker = $worker".to_string());
    }
    if filter.orchestrator.is_some() {
        conditions.push("orchestrator = $orchestrator".to_string());
    }

    if !conditions.is_empty() {
        query.push_str(" WHERE ");
        query.push_str(&conditions.join(" AND "));
    }

    query.push_str(" ORDER BY created_at DESC");

    if let Some(limit) = filter.limit {
        query.push_str(&format!(" LIMIT {limit}"));
    }

    let mut q_builder = db.query(query);

    if let Some(ref s) = filter.status {
        q_builder = q_builder.bind(("status", s.clone()));
    }
    if let Some(ref w) = filter.worker {
        q_builder = q_builder.bind(("worker", w.clone()));
    }
    if let Some(ref o) = filter.orchestrator {
        q_builder = q_builder.bind(("orchestrator", o.clone()));
    }

    let rows: Vec<SessionRowWithId> = q_builder.await?.check()?.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            r.to_record()
                .map_err(|e| warn!(error = %e, "skipping unreadable session row"))
                .ok()
        })
        .collect())
}
