//! Envelope history + threading storage methods for SurrealDB.
//!
//! Split from `surreal.rs` to keep files under 400 LOC. Implements the
//! message-history and conversation-threading methods of the `Storage`
//! trait on `SurrealStorage`.
//!
//! Reply links use the stored `reply_to` *field* (the parent's raw message
//! id, indexed by `idx_env_reply_to`). The `reply_to` graph edge is still
//! written for graph queries, but Surreal 2.x inbound traversal
//! (`<-reply_to<-envelopes`) does not reliably match, so reads don't use it.

use std::collections::HashSet;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use surrealdb::RecordId;
use tracing::{debug, warn};

use crate::protocol::Envelope;
use crate::storage::dbtime::{db_time, DbTime};
use crate::storage::surreal::Db;
use crate::storage::{EnvelopeRecord, HistoryQuery};

/// Columns selected for every envelope read.
const COLUMNS: &str =
    "id, from_identity, channel, to_identity, timestamp, kind, reply_to, payload, stored_at";

/// Internal row type for envelopes table (without ID — ID is the record key).
#[derive(Debug, Serialize, Deserialize)]
struct EnvelopeRow {
    from_identity: String,
    channel: String,
    to_identity: Option<String>,
    timestamp: DbTime,
    kind: String,
    reply_to: Option<String>,
    payload: serde_json::Value,
    stored_at: DbTime,
}

/// Row type that includes the record ID (for queries that return it).
#[derive(Debug, Deserialize)]
struct EnvelopeRowWithId {
    id: RecordId,
    from_identity: String,
    channel: String,
    #[serde(default)]
    to_identity: Option<String>,
    timestamp: DbTime,
    kind: String,
    #[serde(default)]
    reply_to: Option<String>,
    #[serde(default)]
    payload: serde_json::Value,
    stored_at: DbTime,
}

impl EnvelopeRowWithId {
    /// The raw message id (`meta.id`), i.e. the record key without the
    /// `envelopes:` table prefix or SurrealDB quoting.
    fn key(&self) -> String {
        String::try_from(self.id.key().clone())
            .unwrap_or_else(|_| self.id.key().to_string().trim_matches('`').to_string())
    }

    fn into_record(self) -> EnvelopeRecord {
        EnvelopeRecord {
            id: self.id.to_string(),
            from_identity: self.from_identity,
            channel: self.channel,
            to_identity: self.to_identity,
            timestamp: self.timestamp.into(),
            kind: self.kind,
            reply_to: self.reply_to,
            payload: self.payload,
            stored_at: self.stored_at.into(),
        }
    }
}

fn into_records(rows: Vec<EnvelopeRowWithId>) -> Vec<EnvelopeRecord> {
    rows.into_iter()
        .map(EnvelopeRowWithId::into_record)
        .collect()
}

// ── Message History ─────────────────────────────────────────

pub async fn store_envelope(db: &Db, env: &Envelope) -> Result<()> {
    debug!(id = %env.meta.id, "storing envelope");

    let record = EnvelopeRecord::from_envelope(env);
    let row = EnvelopeRow {
        from_identity: record.from_identity,
        channel: record.channel,
        to_identity: record.to_identity,
        timestamp: db_time(record.timestamp),
        kind: record.kind,
        reply_to: record.reply_to,
        payload: record.payload,
        stored_at: db_time(record.stored_at),
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

    let mut query = format!("SELECT {COLUMNS} FROM envelopes");
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
        q_builder = q_builder.bind(("since", db_time(since)));
    }
    if let Some(until) = q.until {
        q_builder = q_builder.bind(("until", db_time(until)));
    }

    let rows: Vec<EnvelopeRowWithId> = q_builder.await?.check()?.take(0)?;
    Ok(into_records(rows))
}

pub async fn get_envelope(db: &Db, id: &str) -> Result<Option<EnvelopeRecord>> {
    debug!(%id, "getting envelope");

    let rows: Vec<EnvelopeRowWithId> = db
        .query(format!(
            "SELECT {COLUMNS} FROM type::thing('envelopes', $id)"
        ))
        .bind(("id", id.to_string()))
        .await?
        .check()?
        .take(0)?;
    Ok(rows.into_iter().next().map(EnvelopeRowWithId::into_record))
}

// ── Conversation Threading ──────────────────────────────────

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

/// Full reply tree under `root_id`, breadth-first: the root (if stored)
/// first, then each level of replies in timestamp order. Walks at most
/// `max_depth` levels of replies and returns at most `limit` envelopes
/// (`None` = unbounded). Each level is one indexed `reply_to IN $ids` query.
pub async fn get_thread(
    db: &Db,
    root_id: &str,
    max_depth: usize,
    limit: Option<usize>,
) -> Result<Vec<EnvelopeRecord>> {
    debug!(%root_id, max_depth, ?limit, "fetching thread");
    if limit == Some(0) {
        return Ok(vec![]);
    }

    let mut out: Vec<EnvelopeRowWithId> = db
        .query(format!(
            "SELECT {COLUMNS} FROM type::thing('envelopes', $root)"
        ))
        .bind(("root", root_id.to_string()))
        .await?
        .check()?
        .take(0)?;

    let mut seen: HashSet<String> = HashSet::from([root_id.to_string()]);
    let mut frontier: Vec<String> = vec![root_id.to_string()];

    for _ in 0..max_depth {
        let remaining = limit.map(|l| l.saturating_sub(out.len()));
        if frontier.is_empty() || remaining == Some(0) {
            break;
        }
        let mut sql = format!(
            "SELECT {COLUMNS} FROM envelopes WHERE reply_to IN $ids ORDER BY timestamp ASC"
        );
        if let Some(remaining) = remaining {
            sql.push_str(&format!(" LIMIT {remaining}"));
        }
        let level: Vec<EnvelopeRowWithId> = db
            .query(sql)
            .bind(("ids", std::mem::take(&mut frontier)))
            .await?
            .check()?
            .take(0)?;

        for row in level {
            let key = row.key();
            // `seen` guards against cycles (a reply_to pointing back up).
            if seen.insert(key.clone()) {
                frontier.push(key);
                out.push(row);
            }
        }
    }

    if let Some(limit) = limit {
        out.truncate(limit);
    }
    Ok(into_records(out))
}

/// Envelopes addressed to `identity` (`kind` message or human) that have no
/// `kind = message` reply pointing at them via `reply_to`. Progress
/// (`status`/`event`) envelopes don't count as answers. Newest first.
///
/// A single query: the correlated subquery is an indexed lookup on
/// `reply_to` (`idx_env_reply_to`), the outer filter uses `idx_env_to`.
pub async fn list_pending(
    db: &Db,
    identity: &str,
    limit: Option<usize>,
) -> Result<Vec<EnvelopeRecord>> {
    debug!(%identity, ?limit, "listing pending messages");

    let mut sql = format!(
        "SELECT {COLUMNS} FROM envelopes \
         WHERE to_identity = $identity \
           AND kind IN ['message', 'human'] \
           AND array::len((SELECT VALUE id FROM envelopes \
                 WHERE reply_to = record::id($parent.id) AND kind = 'message' LIMIT 1)) = 0 \
         ORDER BY timestamp DESC"
    );
    if let Some(limit) = limit {
        sql.push_str(&format!(" LIMIT {limit}"));
    }

    let rows: Vec<EnvelopeRowWithId> = db
        .query(sql)
        .bind(("identity", identity.to_string()))
        .await?
        .check()?
        .take(0)?;
    Ok(into_records(rows))
}
