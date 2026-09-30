//! Agent-registry storage methods for SurrealDB.
//!
//! Split from `surreal.rs` to keep files under 400 LOC. Implements the
//! agent-related methods of the `Storage` trait on `SurrealStorage`.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use surrealdb::RecordId;
use tracing::debug;

use crate::storage::dbtime::{db_now, db_time, metadata_object, DbTime};
use crate::storage::surreal::Db;
use crate::storage::{AgentFilter, AgentRecord};

/// Internal row type for agents table.
/// `id` is the SurrealDB record ID (e.g. `agents:agent-gamma`).
/// Datetimes are native SurrealDB datetimes (see `storage::dbtime`).
#[derive(Debug, Serialize, Deserialize)]
struct AgentRow {
    #[serde(default, skip_serializing)]
    id: Option<RecordId>,
    #[serde(default)]
    identity: String,
    capabilities: Vec<String>,
    last_seen: DbTime,
    registered_at: DbTime,
    #[serde(default)]
    metadata: serde_json::Value,
}

impl AgentRow {
    fn into_record(self) -> AgentRecord {
        let identity = self
            .id
            .as_ref()
            .map(|id| {
                let s = id.key().to_string();
                // Strip SurrealDB backtick quoting from string keys
                s.trim_matches('`').to_string()
            })
            .unwrap_or_default();
        AgentRecord {
            identity,
            capabilities: self.capabilities,
            last_seen: self.last_seen.into(),
            registered_at: self.registered_at.into(),
            metadata: self.metadata,
        }
    }
}

pub async fn register_agent(db: &Db, agent: AgentRecord) -> Result<()> {
    debug!(identity = %agent.identity, "storing agent record");

    let row = AgentRow {
        id: None,
        identity: agent.identity.clone(), // sent as content but ignored on store
        capabilities: agent.capabilities,
        last_seen: db_time(agent.last_seen),
        registered_at: db_time(agent.registered_at),
        metadata: metadata_object(agent.metadata),
    };

    let _: Option<AgentRow> = db
        .upsert(("agents", &agent.identity))
        .content(row)
        .await
        .context("failed to upsert agent")?;

    Ok(())
}

pub async fn deregister_agent(db: &Db, identity: &str) -> Result<()> {
    debug!(%identity, "deregistering agent");

    let _: Option<AgentRow> = db
        .delete(("agents", identity))
        .await
        .context("failed to delete agent")?;

    Ok(())
}

pub async fn touch_agent(db: &Db, identity: &str) -> Result<()> {
    debug!(%identity, "touching agent liveness");

    let _: Option<AgentRow> = db
        .query("UPDATE type::thing('agents', $id) SET last_seen = $now")
        .bind(("id", identity.to_string()))
        .bind(("now", db_now()))
        .await?
        .check()?
        .take(0)?;

    Ok(())
}

pub async fn find_agents(db: &Db, filter: &AgentFilter) -> Result<Vec<AgentRecord>> {
    debug!(?filter, "finding agents");

    let mut query = String::from("SELECT id, * FROM agents");
    let mut conditions: Vec<String> = vec![];

    if filter.alive_within_secs.is_some() {
        conditions.push("last_seen > $cutoff".to_string());
    }
    // Filter capabilities in the query (not after LIMIT) so `limit` counts
    // matching agents only.
    if !filter.capabilities.is_empty() {
        conditions.push("capabilities CONTAINSALL $caps".to_string());
    }

    if !conditions.is_empty() {
        query.push_str(" WHERE ");
        query.push_str(&conditions.join(" AND "));
    }

    if let Some(limit) = filter.limit {
        query.push_str(&format!(" LIMIT {limit}"));
    }

    let mut q_builder = db.query(query);

    if let Some(secs) = filter.alive_within_secs {
        let cutoff = Utc::now() - chrono::Duration::seconds(secs);
        q_builder = q_builder.bind(("cutoff", db_time(cutoff)));
    }
    if !filter.capabilities.is_empty() {
        q_builder = q_builder.bind(("caps", filter.capabilities.clone()));
    }

    let rows: Vec<AgentRow> = q_builder.await?.check()?.take(0)?;
    Ok(rows.into_iter().map(AgentRow::into_record).collect())
}

pub async fn get_agent(db: &Db, identity: &str) -> Result<Option<AgentRecord>> {
    debug!(%identity, "getting agent");

    let mut result = db
        .query("SELECT * FROM type::thing('agents', $id)")
        .bind(("id", identity.to_string()))
        .await?;

    let rows: Vec<AgentRow> = result.take(0)?;
    Ok(rows.into_iter().next().map(AgentRow::into_record))
}
