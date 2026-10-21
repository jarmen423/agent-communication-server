//! Backup export / restore import for SurrealDB storage.
//!
//! [`export_to`] uses `Surreal::export`, which runs inside a single read
//! transaction: the `.surql` file is a consistent snapshot of a **running**
//! hub-server — schema `DEFINE`s plus table data (envelopes, agents,
//! sessions, waves, wave tasks, `reply_to` edges).
//!
//! [`import_from`] replays that file into a **fresh** database path (the
//! export includes `DEFINE TABLE`/`DEFINE FIELD`/`DEFINE INDEX` statements,
//! so importing into an already-migrated DB fails on redefinition).
//! `hub-server restore` refuses a non-empty `--db-path` for this reason.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::storage::surreal::Db;

/// Tables checked after a restore (and reported by `hub-server restore`).
/// `reply_to` has no record table of its own to count meaningfully — it is
/// a relation, verified via `get_thread` in tests.
const COUNTED_TABLES: &[&str] = &[
    "envelopes",
    "agents",
    "sessions",
    "waves",
    "wave_tasks",
    "schema_meta",
];

/// Export the whole database to `path` as SurrealQL. Refuses to overwrite
/// an existing file — backups should be append-only; rotate by name.
/// Returns the bytes written.
pub async fn export_to(db: &Db, path: &Path) -> Result<u64> {
    if path.exists() {
        bail!(
            "backup path '{}' already exists — refusing to overwrite",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            bail!("backup directory '{}' does not exist", parent.display());
        }
    }
    db.export(path.to_path_buf())
        .await
        .with_context(|| format!("SurrealDB export to '{}' failed", path.display()))?;
    let size = std::fs::metadata(path)
        .with_context(|| format!("exported file '{}' missing", path.display()))?
        .len();
    Ok(size)
}

/// Import a `.surql` export into this (fresh) database. The export selects
/// no namespace — data lands in the caller's `use_ns`/`use_db` selection
/// (`nats_hub`/`messaging` via `SurrealStorage`).
pub async fn import_from(db: &Db, path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("backup file '{}' not found", path.display());
    }
    match db.import(path).await {
        Ok(()) => Ok(()),
        Err(e) => {
            // SurrealDB 2.6.5 quirk: `DEFINE FIELD x TYPE array<T>` replayed
            // by import materialises the `x[*]` element field itself, so the
            // export's explicit `x[*]` define fails "already exists". Import
            // is non-atomic and applies every other statement regardless, so
            // "already exists" errors are benign — anything else is real.
            let errs = format!("{e:#}");
            if errs
                .lines()
                .filter(|l| !l.trim().is_empty() && !l.trim().starts_with("Caused by"))
                .all(|l| l.contains("already exists"))
            {
                return Ok(());
            }
            Err(e).with_context(|| format!("SurrealDB import from '{}' failed", path.display()))
        }
    }
}

/// Row counts per table, for restore verification/reporting.
/// Order matches [`COUNTED_TABLES`].
pub async fn table_counts(db: &Db) -> Result<Vec<(String, u64)>> {
    let mut out = Vec::with_capacity(COUNTED_TABLES.len());
    for table in COUNTED_TABLES {
        let rows: Vec<Value> = db
            .query(format!("SELECT count() AS n FROM {table} GROUP ALL"))
            .await?
            .check()?
            .take(0)?;
        let n = rows
            .first()
            .and_then(|r| r.get("n"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0) as u64;
        out.push(((*table).to_string(), n));
    }
    Ok(out)
}
