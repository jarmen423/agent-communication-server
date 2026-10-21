//! File-lock storage methods for SurrealDB.
//!
//! A `locks` row exists only while a lock is held: release and expiry
//! delete it. Datetimes are native SurrealDB datetimes; reads tolerate
//! legacy string rows the same way sessions/waves do.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::storage::dbtime::{db_time, DbTime, StoredTime};
use crate::storage::surreal::Db;
use crate::storage::LockRecord;

#[derive(Debug, Serialize, Deserialize)]
pub struct LockRow {
    pub lock_id: String,
    pub scope: String,
    pub paths: Vec<String>,
    pub holder: String,
    pub reason: Option<String>,
    pub task_id: Option<String>,
    pub ttl_secs: i64,
    pub created_at: DbTime,
    pub expires_at: DbTime,
}

#[derive(Debug, Deserialize)]
pub struct LockRowWithId {
    pub id: surrealdb::RecordId,
    pub lock_id: String,
    pub scope: String,
    pub paths: Vec<String>,
    pub holder: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    pub ttl_secs: i64,
    created_at: StoredTime,
    expires_at: StoredTime,
}

const LOCK_COLUMNS: &str = "id, lock_id, scope, paths, holder, reason, task_id, \
    ttl_secs, created_at, expires_at";

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.is_empty())
}

impl LockRowWithId {
    pub fn to_record(self) -> Result<LockRecord> {
        let record = self.id.to_string();
        Ok(LockRecord {
            created_at: self.created_at.required(&record, "created_at")?,
            expires_at: self.expires_at.required(&record, "expires_at")?,
            lock_id: self.lock_id,
            scope: self.scope,
            paths: self.paths,
            holder: self.holder,
            reason: non_empty(self.reason),
            task_id: non_empty(self.task_id),
            ttl_secs: self.ttl_secs.max(0) as u64,
        })
    }
}

pub fn lock_to_row(record: &LockRecord) -> LockRow {
    LockRow {
        lock_id: record.lock_id.clone(),
        scope: record.scope.clone(),
        paths: record.paths.clone(),
        holder: record.holder.clone(),
        reason: record.reason.clone(),
        task_id: record.task_id.clone(),
        ttl_secs: record.ttl_secs as i64,
        created_at: db_time(record.created_at),
        expires_at: db_time(record.expires_at),
    }
}

/// Store a lock row. The caller (lock service) has already serialized the
/// conflict check; the record id is a fresh uuid, so the upsert is a plain
/// insert in practice.
pub async fn create_lock(db: &Db, lock: LockRecord) -> Result<()> {
    debug!(lock_id = %lock.lock_id, scope = %lock.scope, "storing lock record");
    let row = lock_to_row(&lock);
    let _: Option<LockRow> = db
        .upsert(("locks", &lock.lock_id))
        .content(row)
        .await
        .context("failed to store lock")?;
    Ok(())
}

pub async fn get_lock(db: &Db, lock_id: &str) -> Result<Option<LockRecord>> {
    let rows: Vec<LockRowWithId> = db
        .query(format!(
            "SELECT {LOCK_COLUMNS} FROM type::thing('locks', $id)"
        ))
        .bind(("id", lock_id.to_string()))
        .await?
        .check()?
        .take(0)?;
    rows.into_iter().next().map(|r| r.to_record()).transpose()
}

pub async fn list_locks(db: &Db, scope: Option<&str>) -> Result<Vec<LockRecord>> {
    let mut sql = format!("SELECT {LOCK_COLUMNS} FROM locks");
    if scope.is_some() {
        sql.push_str(" WHERE scope = $scope");
    }
    sql.push_str(" ORDER BY created_at ASC");
    let mut q = db.query(sql);
    if let Some(s) = scope {
        q = q.bind(("scope", s.to_string()));
    }
    let rows: Vec<LockRowWithId> = q.await?.check()?.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            r.to_record()
                .map_err(|e| warn!(error = %e, "skipping unreadable lock row"))
                .ok()
        })
        .collect())
}

/// `false` when the lock didn't exist.
pub async fn delete_lock(db: &Db, lock_id: &str) -> Result<bool> {
    let rows: Vec<serde_json::Value> = db
        .query("DELETE type::thing('locks', $id) RETURN lock_id")
        .bind(("id", lock_id.to_string()))
        .await?
        .check()
        .with_context(|| format!("failed to delete lock '{lock_id}'"))?
        .take(0)?;
    Ok(!rows.is_empty())
}

/// Point update of `expires_at` + `ttl_secs` — `false` when the lock is gone.
pub async fn renew_lock(
    db: &Db,
    lock_id: &str,
    ttl_secs: u64,
    expires_at: DateTime<Utc>,
) -> Result<bool> {
    let rows: Vec<serde_json::Value> = db
        .query(
            "UPDATE type::thing('locks', $id) \
             SET expires_at = $expires, ttl_secs = $ttl RETURN lock_id",
        )
        .bind(("id", lock_id.to_string()))
        .bind(("expires", db_time(expires_at)))
        .bind(("ttl", ttl_secs as i64))
        .await?
        .check()
        .with_context(|| format!("failed to renew lock '{lock_id}'"))?
        .take(0)?;
    Ok(!rows.is_empty())
}
