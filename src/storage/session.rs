//! Session storage methods for SurrealDB.
//!
//! Split from `surreal.rs` to keep files under 400 LOC.
//! Implements the session-related methods of the `Storage` trait
//! on `SurrealStorage`.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::storage::{SessionFilter, SessionRecord};

/// Internal row type for the sessions table.
/// No `id` field — SurrealDB auto-assigns the record ID from the
/// tuple key passed to `.upsert()`.
/// All fields are `String` (not `Option`) because SurrealDB's
/// `.content()` serializer has issues with `Option<T>`.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionRow {
    pub session_id: String,
    pub orchestrator: String,
    pub worker: String,
    pub status: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub provider: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub closed_at: String,
}

/// Row type that includes the SurrealDB record ID (for queries that return it).
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionRowWithId {
    pub id: surrealdb::RecordId,
    pub session_id: String,
    pub orchestrator: String,
    pub worker: String,
    pub status: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub provider: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub closed_at: String,
}

impl SessionRowWithId {
    /// Convert a DB row into a public `SessionRecord`.
    pub fn to_record(self) -> SessionRecord {
        let parse_or_now =
            |s: &str| -> chrono::DateTime<chrono::Utc> { s.parse().unwrap_or_else(|_| Utc::now()) };
        SessionRecord {
            session_id: self.session_id,
            orchestrator: self.orchestrator,
            worker: self.worker,
            status: self.status,
            cwd: if self.cwd.is_empty() {
                None
            } else {
                Some(self.cwd)
            },
            model: if self.model.is_empty() {
                None
            } else {
                Some(self.model)
            },
            provider: if self.provider.is_empty() {
                None
            } else {
                Some(self.provider)
            },
            created_at: parse_or_now(&self.created_at),
            updated_at: parse_or_now(&self.updated_at),
            closed_at: if self.closed_at.is_empty() {
                None
            } else {
                self.closed_at.parse().ok()
            },
            metadata: serde_json::json!({}),
        }
    }
}

/// SQL columns shared between SELECT queries and row conversion.
/// Used to keep query strings DRY across methods.
const SESSION_COLUMNS: &str = "id, session_id, orchestrator, worker, status, \
     cwd, model, provider, created_at, updated_at, closed_at, metadata";

/// Convert a `SessionRecord` into a `SessionRow` for storage.
pub fn session_to_row(record: &SessionRecord) -> SessionRow {
    SessionRow {
        session_id: record.session_id.clone(),
        orchestrator: record.orchestrator.clone(),
        worker: record.worker.clone(),
        status: record.status.clone(),
        cwd: record.cwd.clone().unwrap_or_default(),
        model: record.model.clone().unwrap_or_default(),
        provider: record.provider.clone().unwrap_or_default(),
        created_at: record.created_at.to_rfc3339(),
        updated_at: record.updated_at.to_rfc3339(),
        closed_at: record
            .closed_at
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default(),
    }
}

/// Create a session in the DB.
pub async fn create_session(
    db: &surrealdb::Surreal<surrealdb::engine::local::Db>,
    session: SessionRecord,
) -> Result<()> {
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
pub async fn update_session_status(
    db: &surrealdb::Surreal<surrealdb::engine::local::Db>,
    session_id: &str,
    status: &str,
) -> Result<()> {
    debug!(%session_id, %status, "updating session status");
    let now = Utc::now().to_rfc3339();

    if status == "closed" {
        let _: Option<SessionRow> = db
            .query("UPDATE type::thing('sessions', $id) SET status = $status, updated_at = $now, closed_at = $now")
            .bind(("id", session_id.to_string()))
            .bind(("status", status.to_string()))
            .bind(("now", now))
            .await?
            .take(0)?;
    } else {
        let _: Option<SessionRow> = db
            .query("UPDATE type::thing('sessions', $id) SET status = $status, updated_at = $now")
            .bind(("id", session_id.to_string()))
            .bind(("status", status.to_string()))
            .bind(("now", now))
            .await?
            .take(0)?;
    }
    Ok(())
}

/// Get a single session by ID.
pub async fn get_session(
    db: &surrealdb::Surreal<surrealdb::engine::local::Db>,
    session_id: &str,
) -> Result<Option<SessionRecord>> {
    debug!(%session_id, "getting session");
    let mut result = db
        .query(&format!(
            "SELECT {SESSION_COLUMNS} FROM type::thing('sessions', $id)"
        ))
        .bind(("id", session_id.to_string()))
        .await?;
    let rows: Vec<SessionRowWithId> = result.take(0)?;
    Ok(rows.into_iter().next().map(|r| r.to_record()))
}

/// List sessions matching a filter.
pub async fn list_sessions(
    db: &surrealdb::Surreal<surrealdb::engine::local::Db>,
    filter: &SessionFilter,
) -> Result<Vec<SessionRecord>> {
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

    let rows: Vec<SessionRowWithId> = q_builder.await?.take(0)?;
    Ok(rows.into_iter().map(|r| r.to_record()).collect())
}
