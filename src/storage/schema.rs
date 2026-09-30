//! SurrealDB schema migration for nats-hub.
//!
//! `migrate()` is idempotent and runs on every `hub-server` start:
//!
//! 1. define tables (`IF NOT EXISTS`);
//! 2. if the DB is older than [`SCHEMA_VERSION`], convert legacy rows
//!    (datetimes stored as RFC 3339 strings, `""` for unset optionals,
//!    missing `metadata`) so they satisfy the typed fields;
//! 3. define fields (`OVERWRITE`, so types can evolve) and indexes
//!    (`IF NOT EXISTS`, so big tables are not re-indexed on every start);
//! 4. record the schema version.
//!
//! Every statement is checked. Any failure aborts the migration with an error
//! naming the statement, so `hub-server` fails fast instead of running on a
//! half-applied schema.

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::storage::surreal::Db;

/// Current schema version, stored in `schema_meta:nats_hub`.
/// v1 = the original schema (never actually applied: it used `AT` instead
/// of `ON`). v2 = typed fields applied, native datetimes, reply/recipient
/// indexes.
pub const SCHEMA_VERSION: i64 = 2;

const TABLES: &[&str] = &[
    "DEFINE TABLE IF NOT EXISTS agents SCHEMALESS",
    "DEFINE TABLE IF NOT EXISTS envelopes SCHEMALESS",
    "DEFINE TABLE IF NOT EXISTS reply_to SCHEMALESS TYPE RELATION FROM envelopes TO envelopes",
    "DEFINE TABLE IF NOT EXISTS sessions SCHEMALESS",
    "DEFINE TABLE IF NOT EXISTS waves SCHEMALESS",
    "DEFINE TABLE IF NOT EXISTS wave_tasks SCHEMALESS",
    "DEFINE TABLE IF NOT EXISTS schema_meta SCHEMALESS",
];

/// Required datetime columns, per table (converted from legacy strings).
const DATETIME_FIELDS: &[(&str, &str)] = &[
    ("agents", "last_seen"),
    ("agents", "registered_at"),
    ("envelopes", "timestamp"),
    ("envelopes", "stored_at"),
    ("sessions", "created_at"),
    ("sessions", "updated_at"),
    ("waves", "created_at"),
    ("wave_tasks", "created_at"),
];

/// Optional datetime columns (legacy rows used `""` for "unset").
const OPTIONAL_DATETIME_FIELDS: &[(&str, &str)] = &[
    ("sessions", "closed_at"),
    ("waves", "closed_at"),
    ("wave_tasks", "started_at"),
    ("wave_tasks", "completed_at"),
];

/// Tables with a `metadata` object column (legacy rows lack it or hold null).
const METADATA_TABLES: &[&str] = &["agents", "sessions", "waves"];

const FIELDS_AND_INDEXES: &[&str] = &[
    // Agents
    "DEFINE FIELD OVERWRITE identity      ON TABLE agents TYPE string",
    "DEFINE FIELD OVERWRITE capabilities  ON TABLE agents TYPE array<string>",
    "DEFINE FIELD OVERWRITE last_seen     ON TABLE agents TYPE datetime",
    "DEFINE FIELD OVERWRITE registered_at ON TABLE agents TYPE datetime",
    "DEFINE FIELD OVERWRITE metadata      ON TABLE agents TYPE object",
    "DEFINE INDEX IF NOT EXISTS idx_agents_last_seen ON TABLE agents COLUMNS last_seen",
    // Envelopes. `payload` is free-form JSON (any shape), per the protocol.
    "DEFINE FIELD OVERWRITE from_identity ON TABLE envelopes TYPE string",
    "DEFINE FIELD OVERWRITE channel       ON TABLE envelopes TYPE string",
    "DEFINE FIELD OVERWRITE to_identity   ON TABLE envelopes TYPE option<string>",
    "DEFINE FIELD OVERWRITE timestamp     ON TABLE envelopes TYPE datetime",
    "DEFINE FIELD OVERWRITE kind          ON TABLE envelopes TYPE string",
    "DEFINE FIELD OVERWRITE reply_to      ON TABLE envelopes TYPE option<string>",
    "DEFINE FIELD OVERWRITE payload       ON TABLE envelopes TYPE any",
    "DEFINE FIELD OVERWRITE stored_at     ON TABLE envelopes TYPE datetime",
    "DEFINE INDEX IF NOT EXISTS idx_env_channel_time ON TABLE envelopes COLUMNS channel, timestamp",
    "DEFINE INDEX IF NOT EXISTS idx_env_from_time    ON TABLE envelopes COLUMNS from_identity, timestamp",
    "DEFINE INDEX IF NOT EXISTS idx_env_kind_time    ON TABLE envelopes COLUMNS kind, timestamp",
    "DEFINE INDEX IF NOT EXISTS idx_env_to           ON TABLE envelopes COLUMNS to_identity",
    "DEFINE INDEX IF NOT EXISTS idx_env_reply_to     ON TABLE envelopes COLUMNS reply_to",
    "DEFINE INDEX IF NOT EXISTS idx_env_time         ON TABLE envelopes COLUMNS timestamp",
    // Sessions
    "DEFINE FIELD OVERWRITE session_id   ON TABLE sessions TYPE string",
    "DEFINE FIELD OVERWRITE orchestrator ON TABLE sessions TYPE string",
    "DEFINE FIELD OVERWRITE worker       ON TABLE sessions TYPE string",
    "DEFINE FIELD OVERWRITE status       ON TABLE sessions TYPE string",
    "DEFINE FIELD OVERWRITE cwd          ON TABLE sessions TYPE option<string>",
    "DEFINE FIELD OVERWRITE model        ON TABLE sessions TYPE option<string>",
    "DEFINE FIELD OVERWRITE provider     ON TABLE sessions TYPE option<string>",
    "DEFINE FIELD OVERWRITE created_at   ON TABLE sessions TYPE datetime",
    "DEFINE FIELD OVERWRITE updated_at   ON TABLE sessions TYPE datetime",
    "DEFINE FIELD OVERWRITE closed_at    ON TABLE sessions TYPE option<datetime>",
    "DEFINE FIELD OVERWRITE metadata     ON TABLE sessions TYPE object",
    "DEFINE INDEX IF NOT EXISTS idx_sessions_status ON TABLE sessions COLUMNS status",
    "DEFINE INDEX IF NOT EXISTS idx_sessions_worker ON TABLE sessions COLUMNS worker, status",
    // Waves
    "DEFINE FIELD OVERWRITE wave_id      ON TABLE waves TYPE string",
    "DEFINE FIELD OVERWRITE goal         ON TABLE waves TYPE string",
    "DEFINE FIELD OVERWRITE status       ON TABLE waves TYPE string",
    "DEFINE FIELD OVERWRITE orchestrator ON TABLE waves TYPE string",
    "DEFINE FIELD OVERWRITE created_at   ON TABLE waves TYPE datetime",
    "DEFINE FIELD OVERWRITE closed_at    ON TABLE waves TYPE option<datetime>",
    "DEFINE FIELD OVERWRITE metadata     ON TABLE waves TYPE object",
    "DEFINE INDEX IF NOT EXISTS idx_waves_status ON TABLE waves COLUMNS status",
    // Wave tasks
    "DEFINE FIELD OVERWRITE wave_id      ON TABLE wave_tasks TYPE string",
    "DEFINE FIELD OVERWRITE task_id      ON TABLE wave_tasks TYPE string",
    "DEFINE FIELD OVERWRITE worker       ON TABLE wave_tasks TYPE string",
    "DEFINE FIELD OVERWRITE goal         ON TABLE wave_tasks TYPE string",
    "DEFINE FIELD OVERWRITE status       ON TABLE wave_tasks TYPE string",
    "DEFINE FIELD OVERWRITE write_scope  ON TABLE wave_tasks TYPE array<string>",
    "DEFINE FIELD OVERWRITE dependencies ON TABLE wave_tasks TYPE array<string>",
    "DEFINE FIELD OVERWRITE handoff_path ON TABLE wave_tasks TYPE option<string>",
    "DEFINE FIELD OVERWRITE verify_cmd   ON TABLE wave_tasks TYPE option<string>",
    "DEFINE FIELD OVERWRITE created_at   ON TABLE wave_tasks TYPE datetime",
    "DEFINE FIELD OVERWRITE started_at   ON TABLE wave_tasks TYPE option<datetime>",
    "DEFINE FIELD OVERWRITE completed_at ON TABLE wave_tasks TYPE option<datetime>",
    "DEFINE FIELD OVERWRITE result       ON TABLE wave_tasks TYPE option<string>",
    "DEFINE INDEX IF NOT EXISTS idx_wt_wave_status ON TABLE wave_tasks COLUMNS wave_id, status",
    // Schema bookkeeping
    "DEFINE FIELD OVERWRITE version ON TABLE schema_meta TYPE int",
];

/// Run one statement and fail on *any* error, including per-statement errors
/// that SurrealDB reports inside an otherwise successful response.
pub(crate) async fn run_checked(db: &Db, stmt: &str) -> Result<()> {
    checked(db, stmt.to_string())
        .await
        .with_context(|| format!("schema migration statement failed: {stmt}"))?;
    Ok(())
}

/// Execute a query and surface per-statement errors as `Err`.
async fn checked(db: &Db, sql: String) -> surrealdb::Result<surrealdb::Response> {
    db.query(sql).await?.check()
}

async fn stored_version(db: &Db) -> Result<i64> {
    let versions: Vec<i64> = checked(db, "SELECT VALUE version FROM schema_meta:nats_hub".into())
        .await
        .context("failed to read schema version")?
        .take(0)
        .context("failed to decode schema version")?;
    Ok(versions.into_iter().next().unwrap_or(0))
}

/// Statements converting pre-v2 rows so they satisfy the typed fields.
/// Values that cannot be converted are left in place (and reported by
/// [`report_unconverted`]) rather than overwritten.
fn legacy_conversions() -> Vec<String> {
    let mut stmts = Vec::new();
    for (table, field) in DATETIME_FIELDS.iter().chain(OPTIONAL_DATETIME_FIELDS) {
        stmts.push(format!(
            "UPDATE {table} SET {field} = <datetime> {field} \
             WHERE type::is::string({field}) AND string::is::datetime({field})"
        ));
    }
    for (table, field) in OPTIONAL_DATETIME_FIELDS {
        stmts.push(format!(
            "UPDATE {table} SET {field} = NONE WHERE {field} = '' OR {field} = NULL"
        ));
    }
    for table in METADATA_TABLES {
        stmts.push(format!(
            "UPDATE {table} SET metadata = {{}} WHERE !type::is::object(metadata)"
        ));
    }
    stmts
}

#[derive(serde::Deserialize)]
struct Count {
    count: i64,
}

/// Warn about legacy values the conversion could not fix. Those rows stay
/// readable (reads report them) but updates to them will be rejected.
async fn report_unconverted(db: &Db) -> Result<()> {
    for (table, field) in DATETIME_FIELDS.iter().chain(OPTIONAL_DATETIME_FIELDS) {
        let sql = format!("SELECT count() FROM {table} WHERE type::is::string({field}) GROUP ALL");
        let counts: Vec<Count> = checked(db, sql)
            .await
            .with_context(|| format!("failed to audit {table}.{field}"))?
            .take(0)?;
        let n = counts.first().map_or(0, |c| c.count);
        if n > 0 {
            warn!(
                %table, %field, rows = n,
                "legacy rows hold a non-datetime value that could not be converted; \
                 they stay readable, but updates to them will fail until fixed"
            );
        }
    }
    Ok(())
}

/// Apply the schema (see module docs). Returns an error on the first failed
/// statement.
pub async fn migrate(db: &Db) -> Result<()> {
    info!("running SurrealDB schema migration");

    for stmt in TABLES {
        run_checked(db, stmt).await?;
    }

    let from = stored_version(db).await?;
    let upgrading = from < SCHEMA_VERSION;
    if upgrading {
        info!(from, to = SCHEMA_VERSION, "converting legacy rows");
        for stmt in legacy_conversions() {
            run_checked(db, &stmt).await?;
        }
    }

    for stmt in FIELDS_AND_INDEXES {
        run_checked(db, stmt).await?;
    }

    if upgrading {
        report_unconverted(db).await?;
        run_checked(
            db,
            &format!(
                "UPSERT schema_meta:nats_hub SET version = {SCHEMA_VERSION}, \
                 migrated_at = time::now()"
            ),
        )
        .await?;
    }

    info!(
        version = SCHEMA_VERSION,
        "SurrealDB schema migration complete"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use surrealdb::engine::local::Mem;
    use surrealdb::Surreal;

    #[tokio::test]
    async fn run_checked_rejects_bad_statements() {
        let db: Db = Surreal::new::<Mem>(()).await.unwrap();
        db.use_ns("t").use_db("t").await.unwrap();

        // The original bug: `AT` instead of `ON` is a parse error.
        let err = run_checked(&db, "DEFINE FIELD x AT t TYPE string")
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("DEFINE FIELD x AT t"));

        // Statement-level (not parse) errors must fail too.
        run_checked(&db, "DEFINE TABLE t SCHEMALESS").await.unwrap();
        assert!(run_checked(&db, "DEFINE TABLE t SCHEMALESS").await.is_err());
    }

    #[tokio::test]
    async fn migrate_is_idempotent() {
        let db: Db = Surreal::new::<Mem>(()).await.unwrap();
        db.use_ns("t").use_db("t").await.unwrap();
        migrate(&db).await.unwrap();
        migrate(&db).await.unwrap();
        assert_eq!(stored_version(&db).await.unwrap(), SCHEMA_VERSION);
    }
}
