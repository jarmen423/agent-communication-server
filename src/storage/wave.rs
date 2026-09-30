//! Wave storage methods for SurrealDB.
//!
//! Split from `surreal.rs` to keep files under 400 LOC.
//!
//! Datetimes are stored as native SurrealDB datetimes and wave `metadata`
//! is persisted. Reads tolerate legacy rows (RFC 3339 strings, `""` for
//! unset optionals); a corrupt required datetime is reported, never
//! replaced.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::storage::dbtime::{db_now, db_time, metadata_object, DbTime, StoredTime};
use crate::storage::surreal::Db;
use crate::storage::{WaveRecord, WaveTaskRecord};

#[derive(Debug, Serialize, Deserialize)]
pub struct WaveRow {
    pub wave_id: String,
    pub goal: String,
    pub status: String,
    pub orchestrator: String,
    pub created_at: DbTime,
    pub closed_at: Option<DbTime>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct WaveRowWithId {
    pub id: surrealdb::RecordId,
    pub wave_id: String,
    pub goal: String,
    pub status: String,
    pub orchestrator: String,
    created_at: StoredTime,
    #[serde(default)]
    closed_at: Option<StoredTime>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaveTaskRow {
    pub wave_id: String,
    pub task_id: String,
    pub worker: String,
    pub goal: String,
    pub status: String,
    pub write_scope: Vec<String>,
    pub dependencies: Vec<String>,
    pub handoff_path: Option<String>,
    pub verify_cmd: Option<String>,
    pub created_at: DbTime,
    pub started_at: Option<DbTime>,
    pub completed_at: Option<DbTime>,
    pub result: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WaveTaskRowWithId {
    pub id: surrealdb::RecordId,
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
    handoff_path: Option<String>,
    #[serde(default)]
    verify_cmd: Option<String>,
    created_at: StoredTime,
    #[serde(default)]
    started_at: Option<StoredTime>,
    #[serde(default)]
    completed_at: Option<StoredTime>,
    #[serde(default)]
    result: Option<String>,
}

const WAVE_COLUMNS: &str =
    "id, wave_id, goal, status, orchestrator, created_at, closed_at, metadata";

const WAVE_TASK_COLUMNS: &str = "id, wave_id, task_id, worker, goal, status, write_scope, \
    dependencies, handoff_path, verify_cmd, created_at, started_at, completed_at, result";

/// Legacy rows used `""` for "unset".
fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.is_empty())
}

/// Convert rows, skipping (and logging) any with a corrupt required datetime.
fn collect_readable<R, T>(rows: Vec<R>, convert: impl Fn(R) -> Result<T>, what: &str) -> Vec<T> {
    rows.into_iter()
        .filter_map(|r| {
            convert(r)
                .map_err(|e| warn!(error = %e, "skipping unreadable {what} row"))
                .ok()
        })
        .collect()
}

impl WaveRowWithId {
    pub fn to_record(self) -> Result<WaveRecord> {
        let record = self.id.to_string();
        Ok(WaveRecord {
            created_at: self.created_at.required(&record, "created_at")?,
            closed_at: StoredTime::optional(self.closed_at, &record, "closed_at"),
            wave_id: self.wave_id,
            goal: self.goal,
            status: self.status,
            orchestrator: self.orchestrator,
            metadata: metadata_object(self.metadata.unwrap_or_default()),
        })
    }
}

impl WaveTaskRowWithId {
    pub fn to_record(self) -> Result<WaveTaskRecord> {
        let record = self.id.to_string();
        Ok(WaveTaskRecord {
            created_at: self.created_at.required(&record, "created_at")?,
            started_at: StoredTime::optional(self.started_at, &record, "started_at"),
            completed_at: StoredTime::optional(self.completed_at, &record, "completed_at"),
            wave_id: self.wave_id,
            task_id: self.task_id,
            worker: self.worker,
            goal: self.goal,
            status: self.status,
            write_scope: self.write_scope,
            dependencies: self.dependencies,
            handoff_path: non_empty(self.handoff_path),
            verify_cmd: non_empty(self.verify_cmd),
            result: non_empty(self.result),
        })
    }
}

pub fn wave_to_row(record: &WaveRecord) -> WaveRow {
    WaveRow {
        wave_id: record.wave_id.clone(),
        goal: record.goal.clone(),
        status: record.status.clone(),
        orchestrator: record.orchestrator.clone(),
        created_at: db_time(record.created_at),
        closed_at: record.closed_at.map(db_time),
        metadata: metadata_object(record.metadata.clone()),
    }
}

pub fn wave_task_to_row(record: &WaveTaskRecord) -> WaveTaskRow {
    WaveTaskRow {
        wave_id: record.wave_id.clone(),
        task_id: record.task_id.clone(),
        worker: record.worker.clone(),
        goal: record.goal.clone(),
        status: record.status.clone(),
        write_scope: record.write_scope.clone(),
        dependencies: record.dependencies.clone(),
        handoff_path: record.handoff_path.clone(),
        verify_cmd: record.verify_cmd.clone(),
        created_at: db_time(record.created_at),
        started_at: record.started_at.map(db_time),
        completed_at: record.completed_at.map(db_time),
        result: record.result.clone(),
    }
}

fn wave_task_key(wave_id: &str, task_id: &str) -> String {
    format!("{wave_id}:{task_id}")
}

pub async fn create_wave(db: &Db, wave: WaveRecord) -> Result<()> {
    debug!(wave_id = %wave.wave_id, "storing wave record");
    let row = wave_to_row(&wave);
    let _: Option<WaveRow> = db
        .upsert(("waves", &wave.wave_id))
        .content(row)
        .await
        .context("failed to create wave")?;
    Ok(())
}

pub async fn update_wave_status(db: &Db, wave_id: &str, status: &str) -> Result<()> {
    debug!(%wave_id, %status, "updating wave status");

    let closing = status == "completed" || status == "failed";
    let sql = if closing {
        "UPDATE type::thing('waves', $id) SET status = $status, closed_at = $now"
    } else {
        "UPDATE type::thing('waves', $id) SET status = $status"
    };
    let mut q = db
        .query(sql)
        .bind(("id", wave_id.to_string()))
        .bind(("status", status.to_string()));
    if closing {
        q = q.bind(("now", db_now()));
    }
    q.await?
        .check()
        .with_context(|| format!("failed to update wave '{wave_id}'"))?;
    Ok(())
}

pub async fn get_wave(db: &Db, wave_id: &str) -> Result<Option<WaveRecord>> {
    let rows: Vec<WaveRowWithId> = db
        .query(format!(
            "SELECT {WAVE_COLUMNS} FROM type::thing('waves', $id)"
        ))
        .bind(("id", wave_id.to_string()))
        .await?
        .check()?
        .take(0)?;
    rows.into_iter().next().map(|r| r.to_record()).transpose()
}

pub async fn list_waves(db: &Db, status: Option<&str>) -> Result<Vec<WaveRecord>> {
    let mut query = format!("SELECT {WAVE_COLUMNS} FROM waves");
    if status.is_some() {
        query.push_str(" WHERE status = $status");
    }
    query.push_str(" ORDER BY created_at DESC");

    let mut q_builder = db.query(query);
    if let Some(s) = status {
        q_builder = q_builder.bind(("status", s.to_string()));
    }

    let rows: Vec<WaveRowWithId> = q_builder.await?.check()?.take(0)?;
    Ok(collect_readable(rows, WaveRowWithId::to_record, "wave"))
}

pub async fn create_wave_task(db: &Db, task: WaveTaskRecord) -> Result<()> {
    debug!(wave_id = %task.wave_id, task_id = %task.task_id, "storing wave task");
    let key = wave_task_key(&task.wave_id, &task.task_id);
    let row = wave_task_to_row(&task);
    let _: Option<WaveTaskRow> = db
        .upsert(("wave_tasks", &key))
        .content(row)
        .await
        .context("failed to create wave task")?;
    Ok(())
}

pub async fn update_wave_task_status(
    db: &Db,
    wave_id: &str,
    task_id: &str,
    status: &str,
    result: Option<&str>,
) -> Result<()> {
    debug!(%wave_id, %task_id, %status, "updating wave task status");
    let key = wave_task_key(wave_id, task_id);

    let mut sql = String::from("UPDATE type::thing('wave_tasks', $id) SET status = $status");
    let stamp = match status {
        "running" => Some("started_at"),
        "done" | "failed" => Some("completed_at"),
        _ => None,
    };
    if let Some(field) = stamp {
        sql.push_str(&format!(", {field} = $now"));
    }
    // A result is only recorded on completion (matches the original behavior).
    let result = result.filter(|_| stamp == Some("completed_at"));
    if result.is_some() {
        sql.push_str(", result = $result");
    }

    let mut q = db
        .query(sql)
        .bind(("id", key))
        .bind(("status", status.to_string()));
    if stamp.is_some() {
        q = q.bind(("now", db_now()));
    }
    if let Some(r) = result {
        q = q.bind(("result", r.to_string()));
    }
    q.await?
        .check()
        .with_context(|| format!("failed to update wave task '{wave_id}/{task_id}'"))?;
    Ok(())
}

pub async fn get_wave_task(
    db: &Db,
    wave_id: &str,
    task_id: &str,
) -> Result<Option<WaveTaskRecord>> {
    let key = wave_task_key(wave_id, task_id);
    let rows: Vec<WaveTaskRowWithId> = db
        .query(format!(
            "SELECT {WAVE_TASK_COLUMNS} FROM type::thing('wave_tasks', $id)"
        ))
        .bind(("id", key))
        .await?
        .check()?
        .take(0)?;
    rows.into_iter().next().map(|r| r.to_record()).transpose()
}

pub async fn list_wave_tasks(db: &Db, wave_id: &str) -> Result<Vec<WaveTaskRecord>> {
    let rows: Vec<WaveTaskRowWithId> = db
        .query(format!(
            "SELECT {WAVE_TASK_COLUMNS} FROM wave_tasks WHERE wave_id = $wave_id ORDER BY created_at ASC"
        ))
        .bind(("wave_id", wave_id.to_string()))
        .await?
        .check()?
        .take(0)?;
    Ok(collect_readable(
        rows,
        WaveTaskRowWithId::to_record,
        "wave task",
    ))
}
