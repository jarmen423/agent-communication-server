//! Identity-manifest storage methods for SurrealDB.
//!
//! One row per identity in the `identities` table, keyed by the identity. The
//! record is kept whole as an object, so a provisioning system's new fields
//! need no schema change. A tombstone has `removed = true` and no record.
//! Retention never touches this table.

use anyhow::{Context, Result};
use nats_hub_client::manifest::ManifestRecord;
use serde::{Deserialize, Serialize};
use surrealdb::RecordId;
use tracing::debug;

use crate::storage::dbtime::{db_now, DbTime};
use crate::storage::surreal::Db;
use crate::storage::ManifestRow;

const TABLE: &str = "identities";

#[derive(Serialize)]
struct StoredRow {
    rev: i64,
    removed: bool,
    record: Option<serde_json::Value>,
    updated_at: DbTime,
}

/// A [`StoredRow`] with its record key, for `INSERT`.
#[derive(Serialize)]
struct KeyedRow {
    id: String,
    #[serde(flatten)]
    row: StoredRow,
}

#[derive(Deserialize)]
struct ReadRow {
    id: RecordId,
    rev: i64,
    #[serde(default)]
    record: Option<serde_json::Value>,
}

impl StoredRow {
    fn new(row: &ManifestRow) -> Result<Self> {
        let record = row
            .record
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .context("failed to encode manifest record")?;
        Ok(Self {
            rev: row.rev,
            removed: row.record.is_none(),
            record,
            updated_at: db_now(),
        })
    }
}

impl ReadRow {
    /// The identity, i.e. the record key without SurrealDB quoting.
    fn identity(&self) -> String {
        String::try_from(self.id.key().clone())
            .unwrap_or_else(|_| self.id.key().to_string().trim_matches('`').to_string())
    }

    /// A row that doesn't decode is an error, not a skipped row: the caller
    /// refuses to start rather than route on a partial manifest.
    fn into_row(self) -> Result<ManifestRow> {
        let identity = self.identity();
        let record = self
            .record
            .map(serde_json::from_value::<ManifestRecord>)
            .transpose()
            .with_context(|| format!("bad stored manifest record for '{identity}'"))?;
        Ok(ManifestRow {
            identity,
            rev: self.rev,
            record,
        })
    }
}

pub async fn load(db: &Db) -> Result<Vec<ManifestRow>> {
    let rows: Vec<ReadRow> = db
        .query(format!("SELECT id, rev, record FROM {TABLE}"))
        .await?
        .check()?
        .take(0)
        .context("failed to read the identity manifest")?;
    rows.into_iter().map(ReadRow::into_row).collect()
}

pub async fn save_row(db: &Db, row: &ManifestRow) -> Result<()> {
    debug!(identity = %row.identity, rev = row.rev, "saving manifest row");
    db.query("UPSERT type::thing($table, $id) CONTENT $row")
        .bind(("table", TABLE))
        .bind(("id", row.identity.clone()))
        .bind(("row", StoredRow::new(row)?))
        .await?
        .check()
        .context("failed to save manifest row")?;
    Ok(())
}

pub async fn replace(db: &Db, rows: &[ManifestRow]) -> Result<()> {
    debug!(rows = rows.len(), "replacing the identity manifest");
    let keyed = rows
        .iter()
        .map(|row| {
            Ok(KeyedRow {
                id: row.identity.clone(),
                row: StoredRow::new(row)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    db.query(format!(
        "BEGIN TRANSACTION; DELETE {TABLE}; INSERT INTO {TABLE} $rows; COMMIT TRANSACTION;"
    ))
    .bind(("rows", keyed))
    .await?
    .check()
    .context("failed to replace the identity manifest")?;
    Ok(())
}
