//! SurrealDB schema migration for nats-hub.
//!
//! Split from `surreal.rs` to keep files under 400 LOC.

use anyhow::Result;
use tracing::{debug, info};

use crate::storage::surreal::Db;

/// Schema statements, applied in order by [`migrate`].
const STATEMENTS: &[&str] = &[
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
    // Waves
    "DEFINE TABLE waves SCHEMALESS",
    "DEFINE FIELD wave_id      AT waves TYPE string",
    "DEFINE FIELD goal         AT waves TYPE string",
    "DEFINE FIELD status       AT waves TYPE string",
    "DEFINE FIELD orchestrator AT waves TYPE string",
    "DEFINE FIELD created_at   AT waves TYPE datetime",
    "DEFINE FIELD closed_at    AT waves TYPE option<datetime>",
    "DEFINE FIELD metadata     AT waves TYPE object",
    "DEFINE INDEX idx_waves_status ON TABLE waves COLUMNS status",
    "DEFINE TABLE wave_tasks SCHEMALESS",
    "DEFINE FIELD wave_id      AT wave_tasks TYPE string",
    "DEFINE FIELD task_id      AT wave_tasks TYPE string",
    "DEFINE FIELD worker       AT wave_tasks TYPE string",
    "DEFINE FIELD goal         AT wave_tasks TYPE string",
    "DEFINE FIELD status       AT wave_tasks TYPE string",
    "DEFINE FIELD write_scope  AT wave_tasks TYPE array<string>",
    "DEFINE FIELD dependencies AT wave_tasks TYPE array<string>",
    "DEFINE FIELD handoff_path AT wave_tasks TYPE option<string>",
    "DEFINE FIELD verify_cmd   AT wave_tasks TYPE option<string>",
    "DEFINE FIELD created_at   AT wave_tasks TYPE datetime",
    "DEFINE FIELD started_at   AT wave_tasks TYPE option<datetime>",
    "DEFINE FIELD completed_at AT wave_tasks TYPE option<datetime>",
    "DEFINE FIELD result       AT wave_tasks TYPE option<string>",
    "DEFINE INDEX idx_wt_wave_status ON TABLE wave_tasks COLUMNS wave_id, status",
];

/// Apply the schema.
pub async fn migrate(db: &Db) -> Result<()> {
    info!("running SurrealDB schema migration");

    for q in STATEMENTS {
        if let Err(e) = db.query(*q).await {
            debug!(error = %e, "migration statement (non-fatal): {q}");
        }
    }

    info!("SurrealDB schema migration complete");
    Ok(())
}
