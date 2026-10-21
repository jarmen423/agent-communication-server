//! SurrealDB-backed analytics.
//!
//! Every [`Analytics`] method computes its aggregate inside the database —
//! `GROUP BY` / `GROUP ALL`, `count()`, `math::*`, `array::*` — and returns
//! only the grouped rows. No method loads envelope records into Rust, so
//! memory stays bounded by the number of groups (or `limit`) regardless of
//! how many envelopes the queried range contains.
//!
//! [`SurrealAnalytics::new`] takes the erased `Arc<dyn Storage>` the query
//! API holds and downcasts to the embedded SurrealDB handle. A non-Surreal
//! backend makes every method return an error — this *is* the Surreal
//! implementation; other engines get their own `Analytics` impl.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use surrealdb::sql::Datetime as DbTime;

use crate::analytics::{
    ActivityStats, Analytics, ChannelStats, DataPoint, Interval, LatencyStats, TimeRange,
};
use crate::storage::surreal::Db;
use crate::storage::{Storage, SurrealStorage};

/// Analytics backend over the embedded SurrealDB engine.
pub struct SurrealAnalytics {
    /// Kept for [`SurrealAnalytics::storage`] (tests seed through it) and
    /// for trait ops with no pushdown need.
    storage: Arc<dyn Storage>,
    /// The embedded DB handle, when `storage` is a [`SurrealStorage`].
    db: Option<Db>,
}

/// One aggregate row from a `GROUP BY bucket` query.
#[derive(Debug, Deserialize)]
struct BucketRow {
    bucket: f64,
    count: i64,
}

/// One aggregate row from a `GROUP BY channel` query.
#[derive(Debug, Deserialize)]
struct ChannelRow {
    channel: String,
    messages: i64,
}

/// One scalar aggregate row from a `GROUP ALL` query.
#[derive(Debug, Deserialize)]
struct CountRow {
    #[serde(alias = "count")]
    n: Option<i64>,
}

fn dt(t: DateTime<Utc>) -> DbTime {
    t.into()
}

/// `f64` out of a SurrealDB number; NaN/absent → `fallback`.
fn num_or(v: Option<&Value>, fallback: f64) -> f64 {
    v.and_then(Value::as_f64)
        .filter(|f| f.is_finite())
        .unwrap_or(fallback)
}

/// `u64` out of a SurrealDB int; absent/negative → `fallback`.
fn int_or(v: Option<&Value>, fallback: u64) -> u64 {
    v.and_then(Value::as_i64)
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(fallback)
}

impl SurrealAnalytics {
    /// Wrap a storage backend. Downcasts to [`SurrealStorage`] for the
    /// embedded DB handle; with any other backend the analytics methods
    /// return an error instead of falling back to row-loading.
    pub fn new(storage: Arc<dyn Storage>) -> Self {
        let db = storage
            .as_any()
            .downcast_ref::<SurrealStorage>()
            .map(|s| s.db());
        Self { storage, db }
    }

    /// Access the underlying storage backend (e.g. for seeding in tests).
    pub fn storage(&self) -> &Arc<dyn Storage> {
        &self.storage
    }

    fn db(&self) -> Result<&Db> {
        self.db
            .as_ref()
            .context("SurrealAnalytics requires a SurrealStorage backend")
    }

    /// The bucketed-rate statement (`message_rate`, `error_rate`).
    /// `bucket` is the bucket's offset in `step` units from `since` — the
    /// same indexing the old in-Rust bucketing used, so `DataPoint`
    /// timestamps are unchanged. `extra_where` narrows the rows (e.g.
    /// error events only) and never interpolates user input.
    fn bucket_sql(range: &TimeRange, extra_where: &str) -> String {
        let mut sql = String::from(
            "SELECT math::floor((time::unix(timestamp) - time::unix($since)) / $step) \
             AS bucket, count() AS count \
             FROM envelopes WITH NOINDEX WHERE timestamp > $since",
        );
        if range.until.is_some() {
            sql.push_str(" AND timestamp < $until");
        }
        sql.push_str(extra_where);
        sql.push_str(" GROUP BY bucket ORDER BY bucket ASC");
        sql
    }

    /// `message_rate` / `error_rate` share one shape: envelopes in the
    /// range bucketed by `step` seconds, counted in the DB.
    async fn bucketed_counts(
        &self,
        range: &TimeRange,
        step: i64,
        extra_where: &str,
    ) -> Result<Vec<DataPoint>> {
        let mut q = self
            .db()?
            .query(Self::bucket_sql(range, extra_where))
            .bind(("since", dt(range.since)))
            .bind(("step", step));
        if let Some(until) = range.until {
            q = q.bind(("until", dt(until)));
        }

        let rows: Vec<BucketRow> = q.await?.check()?.take(0)?;
        Ok(rows
            .into_iter()
            .map(|r| DataPoint {
                timestamp: range.since + chrono::Duration::seconds((r.bucket as i64) * step),
                count: r.count.max(0) as u64,
            })
            .collect())
    }

    /// The reply-latency statement. Round-trip lag = reply.timestamp −
    /// original.timestamp, the original being `type::thing(reply_to)`.
    /// Pairs and lags live inside the DB engine; only the six scalars
    /// come back. Exact percentiles still evaluate per-reply values
    /// in-engine (unavoidable), but nothing row-sized crosses into Rust.
    fn latency_sql(range: &TimeRange, channel: Option<&str>) -> String {
        let mut sql = String::from(
            "LET $pairs = (\
               SELECT timestamp AS t, \
                   (SELECT VALUE timestamp FROM ONLY type::thing('envelopes', $parent.reply_to)) AS pt \
               FROM envelopes WITH NOINDEX \
               WHERE reply_to IS NOT NONE AND timestamp > $since",
        );
        if range.until.is_some() {
            sql.push_str(" AND timestamp < $until");
        }
        if channel.is_some() {
            sql.push_str(" AND channel = $channel");
        }
        sql.push_str(
            ");\
             LET $lags = array::map(\
               array::filter($pairs, |$p| $p.pt IS NOT NONE AND $p.t >= $p.pt), \
               |$p| duration::millis($p.t - $p.pt));\
             RETURN {\
               samples: array::len($lags),\
               avg_ms: math::mean($lags),\
               min_ms: math::min($lags),\
               max_ms: math::max($lags),\
               p50_ms: math::percentile($lags, 50),\
               p99_ms: math::percentile($lags, 99)\
             };",
        );
        sql
    }

    /// The sent count of `agent_activity`.
    fn activity_sent_sql(range: &TimeRange) -> String {
        let mut sql = String::from(
            "SELECT count() AS n FROM envelopes WITH NOINDEX \
             WHERE from_identity = $to AND timestamp > $since",
        );
        if range.until.is_some() {
            sql.push_str(" AND timestamp < $until");
        }
        sql.push_str(" GROUP ALL");
        sql
    }

    /// The received-DM half of `agent_activity`.
    fn activity_recv_sql(range: &TimeRange) -> String {
        let mut sql = String::from(
            "SELECT count() AS n FROM envelopes WITH NOINDEX \
             WHERE to_identity = $to AND timestamp > $since",
        );
        if range.until.is_some() {
            sql.push_str(" AND timestamp < $until");
        }
        sql.push_str(" GROUP ALL");
        sql
    }

    /// The event count of `agent_activity` (a filtered count — SurrealDB's
    /// `count(expr)` counts non-NONE rows, which makes `count(cond)`
    /// meaningless: `false` is not NONE).
    fn activity_events_sql(range: &TimeRange) -> String {
        let mut sql = String::from(
            "SELECT count() AS n FROM envelopes WITH NOINDEX \
             WHERE from_identity = $to AND kind = 'event' AND timestamp > $since",
        );
        if range.until.is_some() {
            sql.push_str(" AND timestamp < $until");
        }
        sql.push_str(" GROUP ALL");
        sql
    }

    /// The `channel_hotspots` statement.
    fn hotspots_sql(range: &TimeRange) -> String {
        let mut sql = String::from(
            "SELECT channel, messages FROM (\
               SELECT channel, count() AS messages FROM envelopes \
               WHERE timestamp > $since",
        );
        if range.until.is_some() {
            sql.push_str(" AND timestamp < $until");
        }
        sql.push_str(" GROUP BY channel) ORDER BY messages DESC, channel ASC LIMIT $limit");
        sql
    }

    /// The pending count of `agent_activity`: the same unanswered-DM rule
    /// as `Storage::list_pending`, counted in the DB instead of fetching
    /// the pending records. Snapshot semantics are unchanged — pending
    /// has no time bound.
    const PENDING_SQL: &'static str = "SELECT count() AS n FROM envelopes WITH NOINDEX \
         WHERE to_identity = $to AND kind IN ['message', 'human'] \
           AND array::len((SELECT VALUE id FROM envelopes \
                 WHERE reply_to = record::id($parent.id) AND kind = 'message' LIMIT 1)) = 0 \
         GROUP ALL";
}

#[async_trait]
impl Analytics for SurrealAnalytics {
    async fn message_rate(&self, range: &TimeRange, group_by: Interval) -> Result<Vec<DataPoint>> {
        self.bucketed_counts(range, group_by.as_secs(), "").await
    }

    async fn latency_stats(
        &self,
        channel: Option<&str>,
        range: &TimeRange,
    ) -> Result<LatencyStats> {
        let mut q = self
            .db()?
            .query(Self::latency_sql(range, channel))
            .bind(("since", dt(range.since)));
        if let Some(until) = range.until {
            q = q.bind(("until", dt(until)));
        }
        if let Some(c) = channel {
            q = q.bind(("channel", c.to_string()));
        }

        let row: Option<Value> = q.await?.check()?.take(2)?;
        let Some(row) = row else {
            return Ok(LatencyStats {
                samples: 0,
                avg_ms: 0.0,
                min_ms: 0.0,
                max_ms: 0.0,
                p50_ms: 0.0,
                p99_ms: 0.0,
            });
        };
        let get = |k: &str| row.get(k);
        Ok(LatencyStats {
            samples: int_or(get("samples"), 0),
            avg_ms: num_or(get("avg_ms"), 0.0),
            min_ms: num_or(get("min_ms"), 0.0),
            max_ms: num_or(get("max_ms"), 0.0),
            p50_ms: num_or(get("p50_ms"), 0.0),
            p99_ms: num_or(get("p99_ms"), 0.0),
        })
    }

    async fn agent_activity(&self, identity: &str, range: &TimeRange) -> Result<ActivityStats> {
        let mut q = self
            .db()?
            .query(Self::activity_sent_sql(range))
            .bind(("to", identity.to_string()))
            .bind(("since", dt(range.since)));
        if let Some(until) = range.until {
            q = q.bind(("until", dt(until)));
        }
        let sent_rows: Vec<CountRow> = q.await?.check()?.take(0)?;
        let sent = sent_rows.first().and_then(|r| r.n).unwrap_or(0).max(0) as u64;

        let mut q = self
            .db()?
            .query(Self::activity_events_sql(range))
            .bind(("to", identity.to_string()))
            .bind(("since", dt(range.since)));
        if let Some(until) = range.until {
            q = q.bind(("until", dt(until)));
        }
        let event_rows: Vec<CountRow> = q.await?.check()?.take(0)?;
        let events = event_rows.first().and_then(|r| r.n).unwrap_or(0).max(0) as u64;

        let mut q = self
            .db()?
            .query(Self::activity_recv_sql(range))
            .bind(("to", identity.to_string()))
            .bind(("since", dt(range.since)));
        if let Some(until) = range.until {
            q = q.bind(("until", dt(until)));
        }
        let recv: Vec<CountRow> = q.await?.check()?.take(0)?;

        // Same unanswered-DM rule as `list_pending`, counted in the DB
        // instead of fetching the pending records. Snapshot semantics are
        // unchanged: pending has no time bound.
        let pending_rows: Vec<CountRow> = self
            .db()?
            .query(Self::PENDING_SQL)
            .bind(("to", identity.to_string()))
            .await?
            .check()?
            .take(0)?;

        Ok(ActivityStats {
            identity: identity.to_string(),
            sent,
            received_dm: recv.first().and_then(|r| r.n).unwrap_or(0).max(0) as u64,
            events,
            pending: pending_rows.first().and_then(|r| r.n).unwrap_or(0).max(0) as u64,
        })
    }

    async fn channel_hotspots(&self, range: &TimeRange, limit: usize) -> Result<Vec<ChannelStats>> {
        let mut q = self
            .db()?
            .query(Self::hotspots_sql(range))
            .bind(("since", dt(range.since)))
            .bind(("limit", limit as i64));
        if let Some(until) = range.until {
            q = q.bind(("until", dt(until)));
        }

        let rows: Vec<ChannelRow> = q.await?.check()?.take(0)?;
        Ok(rows
            .into_iter()
            .map(|r| ChannelStats {
                channel: r.channel,
                messages: r.messages.max(0) as u64,
            })
            .collect())
    }

    async fn error_rate(&self, range: &TimeRange, group_by: Interval) -> Result<Vec<DataPoint>> {
        self.bucketed_counts(
            range,
            group_by.as_secs(),
            " AND kind = 'event' AND payload.event_type = 'error'",
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    //! Query-shape assertions: every statement the analytics backend
    //! emits must aggregate inside SurrealQL and must never project
    //! envelope columns (the row-loading shape this file replaced).
    //! `tests/analytics.rs` exercises the same statements end to end.
    use super::*;

    #[test]
    fn aggregates_are_pushed_down() {
        let range = TimeRange::bounded(Utc::now() - chrono::Duration::hours(1), Utc::now());

        for stmt in [
            SurrealAnalytics::bucket_sql(&range, ""),
            SurrealAnalytics::bucket_sql(&range, " AND kind = 'event'"),
            SurrealAnalytics::latency_sql(&range, Some("agents.tasks")),
            SurrealAnalytics::latency_sql(&range, None),
            SurrealAnalytics::activity_sent_sql(&range),
            SurrealAnalytics::activity_recv_sql(&range),
            SurrealAnalytics::activity_events_sql(&range),
            SurrealAnalytics::hotspots_sql(&range),
            SurrealAnalytics::PENDING_SQL.to_string(),
        ] {
            assert!(
                stmt.contains("count(") || stmt.contains("math::") || stmt.contains("GROUP"),
                "must aggregate in the DB: {stmt}"
            );
            // Selecting payload/from_identity/... per envelope is the
            // row-loading shape this file replaced.
            for row_col in ["payload,", "from_identity,", "reply_to,"] {
                assert!(
                    !stmt.contains(row_col),
                    "looks like a row load ({row_col}): {stmt}"
                );
            }
        }
    }
}
