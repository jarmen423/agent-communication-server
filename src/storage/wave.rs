//! Wave storage methods for SurrealDB.
//!
//! Split from `surreal.rs` to keep files under 400 LOC.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::storage::{WaveRecord, WaveTaskRecord};

#[derive(Debug, Serialize, Deserialize)]
pub struct WaveRow {
    pub wave_id: String,
    pub goal: String,
    pub status: String,
    pub orchestrator: String,
    pub created_at: String,
    #[serde(default)]
    pub closed_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaveRowWithId {
    pub id: surrealdb::RecordId,
    pub wave_id: String,
    pub goal: String,
    pub status: String,
    pub orchestrator: String,
    pub created_at: String,
    #[serde(default)]
    pub closed_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaveTaskRow {
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
    pub handoff_path: String,
    #[serde(default)]
    pub verify_cmd: String,
    pub created_at: String,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub completed_at: String,
    #[serde(default)]
    pub result: String,
}

#[derive(Debug, Serialize, Deserialize)]
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
    pub handoff_path: String,
    #[serde(default)]
    pub verify_cmd: String,
    pub created_at: String,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub completed_at: String,
    #[serde(default)]
    pub result: String,
}

const WAVE_COLUMNS: &str =
    "id, wave_id, goal, status, orchestrator, created_at, closed_at, metadata";

const WAVE_TASK_COLUMNS: &str = "id, wave_id, task_id, worker, goal, status, write_scope, \
    dependencies, handoff_path, verify_cmd, created_at, started_at, completed_at, result";

fn parse_or_now(s: &str) -> chrono::DateTime<chrono::Utc> {
    s.parse().unwrap_or_else(|_| Utc::now())
}

impl WaveRowWithId {
    pub fn to_record(self) -> WaveRecord {
        WaveRecord {
            wave_id: self.wave_id,
            goal: self.goal,
            status: self.status,
            orchestrator: self.orchestrator,
            created_at: parse_or_now(&self.created_at),
            closed_at: if self.closed_at.is_empty() {
                None
            } else {
                self.closed_at.parse().ok()
            },
            metadata: serde_json::json!({}),
        }
    }
}

impl WaveTaskRowWithId {
    pub fn to_record(self) -> WaveTaskRecord {
        WaveTaskRecord {
            wave_id: self.wave_id,
            task_id: self.task_id,
            worker: self.worker,
            goal: self.goal,
            status: self.status,
            write_scope: self.write_scope,
            dependencies: self.dependencies,
            handoff_path: if self.handoff_path.is_empty() {
                None
            } else {
                Some(self.handoff_path)
            },
            verify_cmd: if self.verify_cmd.is_empty() {
                None
            } else {
                Some(self.verify_cmd)
            },
            created_at: parse_or_now(&self.created_at),
            started_at: if self.started_at.is_empty() {
                None
            } else {
                self.started_at.parse().ok()
            },
            completed_at: if self.completed_at.is_empty() {
                None
            } else {
                self.completed_at.parse().ok()
            },
            result: if self.result.is_empty() {
                None
            } else {
                Some(self.result)
            },
        }
    }
}

pub fn wave_to_row(record: &WaveRecord) -> WaveRow {
    WaveRow {
        wave_id: record.wave_id.clone(),
        goal: record.goal.clone(),
        status: record.status.clone(),
        orchestrator: record.orchestrator.clone(),
        created_at: record.created_at.to_rfc3339(),
        closed_at: record
            .closed_at
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default(),
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
        handoff_path: record.handoff_path.clone().unwrap_or_default(),
        verify_cmd: record.verify_cmd.clone().unwrap_or_default(),
        created_at: record.created_at.to_rfc3339(),
        started_at: record
            .started_at
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default(),
        completed_at: record
            .completed_at
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default(),
        result: record.result.clone().unwrap_or_default(),
    }
}

fn wave_task_key(wave_id: &str, task_id: &str) -> String {
    format!("{wave_id}:{task_id}")
}

pub async fn create_wave(
    db: &surrealdb::Surreal<surrealdb::engine::local::Db>,
    wave: WaveRecord,
) -> Result<()> {
    debug!(wave_id = %wave.wave_id, "storing wave record");
    let row = wave_to_row(&wave);
    let _: Option<WaveRow> = db
        .upsert(("waves", &wave.wave_id))
        .content(row)
        .await
        .context("failed to create wave")?;
    Ok(())
}

pub async fn update_wave_status(
    db: &surrealdb::Surreal<surrealdb::engine::local::Db>,
    wave_id: &str,
    status: &str,
) -> Result<()> {
    debug!(%wave_id, %status, "updating wave status");
    let now = Utc::now().to_rfc3339();

    if status == "completed" || status == "failed" {
        let _: Option<WaveRow> = db
            .query(
                "UPDATE type::thing('waves', $id) SET status = $status, closed_at = $now",
            )
            .bind(("id", wave_id.to_string()))
            .bind(("status", status.to_string()))
            .bind(("now", now))
            .await?
            .take(0)?;
    } else {
        let _: Option<WaveRow> = db
            .query("UPDATE type::thing('waves', $id) SET status = $status")
            .bind(("id", wave_id.to_string()))
            .bind(("status", status.to_string()))
            .await?
            .take(0)?;
    }
    Ok(())
}

pub async fn get_wave(
    db: &surrealdb::Surreal<surrealdb::engine::local::Db>,
    wave_id: &str,
) -> Result<Option<WaveRecord>> {
    let mut result = db
        .query(&format!(
            "SELECT {WAVE_COLUMNS} FROM type::thing('waves', $id)"
        ))
        .bind(("id", wave_id.to_string()))
        .await?;
    let rows: Vec<WaveRowWithId> = result.take(0)?;
    Ok(rows.into_iter().next().map(|r| r.to_record()))
}

pub async fn list_waves(
    db: &surrealdb::Surreal<surrealdb::engine::local::Db>,
    status: Option<&str>,
) -> Result<Vec<WaveRecord>> {
    let mut query = format!("SELECT {WAVE_COLUMNS} FROM waves");
    if status.is_some() {
        query.push_str(" WHERE status = $status");
    }
    query.push_str(" ORDER BY created_at DESC");

    let mut q_builder = db.query(query);
    if let Some(s) = status {
        q_builder = q_builder.bind(("status", s.to_string()));
    }

    let rows: Vec<WaveRowWithId> = q_builder.await?.take(0)?;
    Ok(rows.into_iter().map(|r| r.to_record()).collect())
}

pub async fn create_wave_task(
    db: &surrealdb::Surreal<surrealdb::engine::local::Db>,
    task: WaveTaskRecord,
) -> Result<()> {
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
    db: &surrealdb::Surreal<surrealdb::engine::local::Db>,
    wave_id: &str,
    task_id: &str,
    status: &str,
    result: Option<&str>,
) -> Result<()> {
    debug!(%wave_id, %task_id, %status, "updating wave task status");
    let key = wave_task_key(wave_id, task_id);
    let now = Utc::now().to_rfc3339();

    match status {
        "running" => {
            let _: Option<WaveTaskRow> = db
                .query(
                    "UPDATE type::thing('wave_tasks', $id) SET status = $status, started_at = $now",
                )
                .bind(("id", key))
                .bind(("status", status.to_string()))
                .bind(("now", now))
                .await?
                .take(0)?;
        }
        "done" | "failed" => {
            let mut query = String::from(
                "UPDATE type::thing('wave_tasks', $id) SET status = $status, completed_at = $now",
            );
            if result.is_some() {
                query.push_str(", result = $result");
            }
            let mut q = db
                .query(&query)
                .bind(("id", key))
                .bind(("status", status.to_string()))
                .bind(("now", now));
            if let Some(r) = result {
                q = q.bind(("result", r.to_string()));
            }
            let _: Option<WaveTaskRow> = q.await?.take(0)?;
        }
        _ => {
            let _: Option<WaveTaskRow> = db
                .query("UPDATE type::thing('wave_tasks', $id) SET status = $status")
                .bind(("id", key))
                .bind(("status", status.to_string()))
                .await?
                .take(0)?;
        }
    }
    Ok(())
}

pub async fn get_wave_task(
    db: &surrealdb::Surreal<surrealdb::engine::local::Db>,
    wave_id: &str,
    task_id: &str,
) -> Result<Option<WaveTaskRecord>> {
    let key = wave_task_key(wave_id, task_id);
    let mut result = db
        .query(&format!(
            "SELECT {WAVE_TASK_COLUMNS} FROM type::thing('wave_tasks', $id)"
        ))
        .bind(("id", key))
        .await?;
    let rows: Vec<WaveTaskRowWithId> = result.take(0)?;
    Ok(rows.into_iter().next().map(|r| r.to_record()))
}

pub async fn list_wave_tasks(
    db: &surrealdb::Surreal<surrealdb::engine::local::Db>,
    wave_id: &str,
) -> Result<Vec<WaveTaskRecord>> {
    let mut result = db
        .query(&format!(
            "SELECT {WAVE_TASK_COLUMNS} FROM wave_tasks WHERE wave_id = $wave_id ORDER BY created_at ASC"
        ))
        .bind(("wave_id", wave_id.to_string()))
        .await?;
    let rows: Vec<WaveTaskRowWithId> = result.take(0)?;
    Ok(rows.into_iter().map(|r| r.to_record()).collect())
}
