//! Envelope history + threading storage methods for SurrealDB.
//!
//! Split from `surreal.rs` to keep files under 400 LOC. Implements the
//! message-history and conversation-threading methods of the `Storage`
//! trait on `SurrealStorage`.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::RecordId;
use tracing::{debug, warn};

use crate::protocol::Envelope;
use crate::storage::surreal::Db;
use crate::storage::{EnvelopeRecord, HistoryQuery};

/// Internal row type for envelopes table (without ID — ID is the record key).
#[derive(Debug, Serialize, Deserialize)]
struct EnvelopeRow {
    from_identity: String,
    channel: String,
    to_identity: Option<String>,
    timestamp: DateTime<Utc>,
    kind: String,
    reply_to: Option<String>,
    payload: serde_json::Value,
    stored_at: DateTime<Utc>,
}

/// Row type that includes the record ID (for queries that return it).
#[derive(Debug, Serialize, Deserialize)]
struct EnvelopeRowWithId {
    id: RecordId,
    from_identity: String,
    channel: String,
    to_identity: Option<String>,
    timestamp: DateTime<Utc>,
    kind: String,
    reply_to: Option<String>,
    payload: serde_json::Value,
    stored_at: DateTime<Utc>,
}

impl EnvelopeRowWithId {
    fn into_record(self) -> EnvelopeRecord {
        EnvelopeRecord {
            id: self.id.to_string(),
            from_identity: self.from_identity,
            channel: self.channel,
            to_identity: self.to_identity,
            timestamp: self.timestamp,
            kind: self.kind,
            reply_to: self.reply_to,
            payload: self.payload,
            stored_at: self.stored_at,
        }
    }
}

/// Strip Surreal record id form (`envelopes:uuid` / backticks) down to the
/// raw uuid string stored in `meta.id` / `reply_to`.
fn raw_envelope_id(id: &str) -> String {
    let s = id.trim().trim_matches('`');
    s.strip_prefix("envelopes:")
        .unwrap_or(s)
        .trim_matches('`')
        .to_string()
}

// ── Message History ─────────────────────────────────────────

pub async fn store_envelope(db: &Db, env: &Envelope) -> Result<()> {
    debug!(id = %env.meta.id, "storing envelope");

    let record = EnvelopeRecord::from_envelope(env);
    let row = EnvelopeRow {
        from_identity: record.from_identity,
        channel: record.channel,
        to_identity: record.to_identity,
        timestamp: record.timestamp,
        kind: record.kind,
        reply_to: record.reply_to,
        payload: record.payload,
        stored_at: record.stored_at,
    };

    let _: Option<EnvelopeRow> = db
        .create(("envelopes", &record.id))
        .content(row)
        .await
        .context("failed to store envelope")?;

    // If this envelope is a reply, create the graph edge
    if let Some(parent_id) = &env.meta.reply_to {
        if let Err(e) = link_reply(db, &env.meta.id, parent_id).await {
            warn!(error = %e, "failed to link reply edge (non-fatal)");
        }
    }

    Ok(())
}

pub async fn query_history(db: &Db, q: &HistoryQuery) -> Result<Vec<EnvelopeRecord>> {
    debug!(?q, "querying history");

    let mut query = String::from("SELECT * FROM envelopes");
    let mut conditions: Vec<String> = vec![];

    if q.channel.is_some() {
        conditions.push("channel = $channel".to_string());
    }
    if q.from.is_some() {
        conditions.push("from_identity = $from".to_string());
    }
    if q.to.is_some() {
        conditions.push("to_identity = $to".to_string());
    }
    if q.kind.is_some() {
        conditions.push("kind = $kind".to_string());
    }
    if q.since.is_some() {
        conditions.push("timestamp > $since".to_string());
    }
    if q.until.is_some() {
        conditions.push("timestamp < $until".to_string());
    }

    if !conditions.is_empty() {
        query.push_str(" WHERE ");
        query.push_str(&conditions.join(" AND "));
    }

    query.push_str(" ORDER BY timestamp DESC");

    if let Some(limit) = q.limit {
        query.push_str(&format!(" LIMIT {limit}"));
    }

    let mut q_builder = db.query(query);

    if let Some(ref ch) = q.channel {
        q_builder = q_builder.bind(("channel", ch.clone()));
    }
    if let Some(ref from) = q.from {
        q_builder = q_builder.bind(("from", from.clone()));
    }
    if let Some(ref to) = q.to {
        q_builder = q_builder.bind(("to", to.clone()));
    }
    if let Some(ref kind) = q.kind {
        q_builder = q_builder.bind(("kind", kind.clone()));
    }
    if let Some(since) = q.since {
        q_builder = q_builder.bind(("since", since));
    }
    if let Some(until) = q.until {
        q_builder = q_builder.bind(("until", until));
    }

    let rows: Vec<EnvelopeRowWithId> = q_builder.await?.take(0)?;
    Ok(rows
        .into_iter()
        .map(EnvelopeRowWithId::into_record)
        .collect())
}

pub async fn get_envelope(db: &Db, id: &str) -> Result<Option<EnvelopeRecord>> {
    debug!(%id, "getting envelope");

    let mut result = db
        .query("SELECT id, from_identity, channel, to_identity, timestamp, kind, reply_to, payload, stored_at FROM type::thing('envelopes', $id)")
        .bind(("id", id.to_string()))
        .await?;

    let rows: Vec<EnvelopeRowWithId> = result.take(0)?;
    Ok(rows.into_iter().next().map(EnvelopeRowWithId::into_record))
}

// ── Conversation Threading (graph) ──────────────────────────

pub async fn link_reply(db: &Db, reply_id: &str, parent_id: &str) -> Result<()> {
    debug!(%reply_id, %parent_id, "linking reply edge");

    // SurrealDB 2.x rejects type::thing() inside the RELATE path; resolve record
    // ids via LET, then RELATE the bound record-id parameters.
    db.query(
        "LET $reply = type::thing('envelopes', $reply_id);
         LET $parent = type::thing('envelopes', $parent_id);
         RELATE $reply->reply_to->$parent",
    )
    .bind(("reply_id", reply_id.to_string()))
    .bind(("parent_id", parent_id.to_string()))
    .await?
    .check()?;

    Ok(())
}

pub async fn get_thread(db: &Db, root_id: &str) -> Result<Vec<EnvelopeRecord>> {
    debug!(%root_id, "fetching thread (graph traversal)");

    // Get root + all envelopes that have reply_to = root_id
    // (Simpler than graph traversal, works reliably in v2)
    let mut result = db
        .query(
            "SELECT id, from_identity, channel, to_identity, timestamp, kind, reply_to, payload, stored_at \
             FROM type::thing('envelopes', $root); \
             SELECT id, from_identity, channel, to_identity, timestamp, kind, reply_to, payload, stored_at \
             FROM envelopes WHERE reply_to = $root"
        )
        .bind(("root", root_id.to_string()))
        .await?;

    let root_rows: Vec<EnvelopeRowWithId> = result.take(0)?;
    let reply_rows: Vec<EnvelopeRowWithId> = result.take(1)?;

    let mut rows = root_rows;
    rows.extend(reply_rows);

    Ok(rows
        .into_iter()
        .map(EnvelopeRowWithId::into_record)
        .collect())
}

pub async fn list_pending(db: &Db, identity: &str) -> Result<Vec<EnvelopeRecord>> {
    debug!(%identity, "listing pending messages");

    // Surreal 2.x graph inbound `<-reply_to<-envelopes IS NONE` does not
    // reliably match unreplied rows (returns empty). Match get_thread():
    // use the stored reply_to *field* (raw uuid string) instead of graph.
    let mut result = db
        .query(
            "SELECT id, from_identity, channel, to_identity, timestamp, kind, reply_to, payload, stored_at \
             FROM envelopes \
             WHERE to_identity = $identity; \
             SELECT VALUE reply_to FROM envelopes WHERE reply_to IS NOT NONE",
        )
        .bind(("identity", identity.to_string()))
        .await?;

    let rows: Vec<EnvelopeRowWithId> = result.take(0)?;
    let answered: Vec<Option<String>> = result.take(1).unwrap_or_default();
    let answered: std::collections::HashSet<String> = answered
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect();

    Ok(rows
        .into_iter()
        .filter(|r| {
            let raw = raw_envelope_id(&r.id.to_string());
            !answered.contains(&raw)
        })
        .map(EnvelopeRowWithId::into_record)
        .collect())
}
