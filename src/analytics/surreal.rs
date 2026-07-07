//! SurrealDB-backed analytics.
//!
//! Derives all metrics from the existing `envelopes` history via the
//! `Storage` trait. Each method performs a single range `query_history`
//! pull and aggregates in Rust — matching the existing "fetch then filter
//! in Rust" pattern used across `crate::storage::surreal`.
//!
//! No new SurrealQL aggregate queries and no schema change.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};

use crate::analytics::{
    ActivityStats, Analytics, ChannelStats, DataPoint, Interval, LatencyStats, TimeRange,
};
use crate::storage::{HistoryQuery, Storage};

/// Analytics backend over any `Storage` implementation.
pub struct SurrealAnalytics {
    storage: Arc<dyn Storage>,
}

impl SurrealAnalytics {
    /// Wrap a storage backend.
    pub fn new(storage: Arc<dyn Storage>) -> Self {
        Self { storage }
    }

    /// Access the underlying storage backend (e.g. for seeding in tests).
    pub fn storage(&self) -> &Arc<dyn Storage> {
        &self.storage
    }

    /// Build a `HistoryQuery` for the given range (since/until only).
    fn range_query(range: &TimeRange) -> HistoryQuery {
        let mut q = HistoryQuery::new().since(range.since);
        if let Some(until) = range.until {
            q = q.until(until);
        }
        q
    }

    /// Extract the bare record key from a SurrealDB `id` string.
    /// `query_history` returns ids as `envelopes:`⟨uuid⟩` (table prefix plus a
    /// backtick-quoted uuid), while `reply_to` is stored as the raw `uuid`.
    /// Normalize so they match.
    fn raw_id(id: &str) -> String {
        let s = id.trim();
        // Strip `table:` prefix if present.
        let body = match s.split_once(':') {
            Some((_, rest)) => rest,
            None => s,
        };
        // Strip surrounding backticks.
        body.trim_matches('`').to_string()
    }

    /// Bucket a set of envelopes by time interval and count per bucket.
    fn bucketize(
        rows: &[crate::storage::EnvelopeRecord],
        range: &TimeRange,
        group_by: Interval,
    ) -> Vec<DataPoint> {
        let step = group_by.as_secs();
        let mut counts: BTreeMap<i64, u64> = BTreeMap::new();
        for row in rows {
            if row.timestamp < range.since {
                continue;
            }
            let offset = (row.timestamp - range.since).num_seconds() / step;
            *counts.entry(offset).or_insert(0) += 1;
        }
        counts
            .into_iter()
            .map(|(offset, count)| DataPoint {
                timestamp: range.since + Duration::seconds(offset * step),
                count,
            })
            .collect()
    }

    /// Percentile over a sorted slice of millisecond samples.
    fn percentile(sorted_ms: &[f64], p: f64) -> f64 {
        if sorted_ms.is_empty() {
            return 0.0;
        }
        if sorted_ms.len() == 1 {
            return sorted_ms[0];
        }
        // Nearest-rank percentile.
        let rank = (p / 100.0 * sorted_ms.len() as f64).ceil() as usize;
        let idx = rank.saturating_sub(1).min(sorted_ms.len() - 1);
        sorted_ms[idx]
    }
}

#[async_trait]
impl Analytics for SurrealAnalytics {
    async fn message_rate(&self, range: &TimeRange, group_by: Interval) -> Result<Vec<DataPoint>> {
        let rows = self
            .storage
            .query_history(&Self::range_query(range))
            .await?;
        Ok(Self::bucketize(&rows, range, group_by))
    }

    async fn latency_stats(
        &self,
        channel: Option<&str>,
        range: &TimeRange,
    ) -> Result<LatencyStats> {
        // Pull the whole range (no channel filter) so originals are included.
        let rows = self
            .storage
            .query_history(&Self::range_query(range))
            .await?;

        // Map original message id -> timestamp for reply correlation.
        // `id` comes back as `envelopes:<uuid>`; normalize to the raw uuid
        // so it matches `reply_to` (stored as the raw uuid).
        let origin_ts: HashMap<String, DateTime<Utc>> = rows
            .iter()
            .map(|r| (Self::raw_id(&r.id), r.timestamp))
            .collect();

        let mut samples_ms: Vec<f64> = Vec::new();
        for row in &rows {
            let Some(reply_to) = &row.reply_to else {
                continue;
            };
            if let Some(c) = channel {
                if row.channel != c {
                    continue;
                }
            }
            if let Some(orig) = origin_ts.get(reply_to) {
                let delta = (row.timestamp - *orig).num_milliseconds();
                if delta >= 0 {
                    samples_ms.push(delta as f64);
                }
            }
        }

        if samples_ms.is_empty() {
            return Ok(LatencyStats {
                samples: 0,
                avg_ms: 0.0,
                min_ms: 0.0,
                max_ms: 0.0,
                p50_ms: 0.0,
                p99_ms: 0.0,
            });
        }

        samples_ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let sum: f64 = samples_ms.iter().sum();
        let avg = sum / samples_ms.len() as f64;
        let min = samples_ms[0];
        let max = samples_ms[samples_ms.len() - 1];
        let p50 = Self::percentile(&samples_ms, 50.0);
        let p99 = Self::percentile(&samples_ms, 99.0);

        Ok(LatencyStats {
            samples: samples_ms.len() as u64,
            avg_ms: avg,
            min_ms: min,
            max_ms: max,
            p50_ms: p50,
            p99_ms: p99,
        })
    }

    async fn agent_activity(&self, identity: &str, range: &TimeRange) -> Result<ActivityStats> {
        let sent_q = Self::range_query(range).from(identity.to_string());
        let sent_rows = self.storage.query_history(&sent_q).await?;
        let events = sent_rows.iter().filter(|r| r.kind == "event").count() as u64;

        let received_q = Self::range_query(range).to(identity.to_string());
        let received_rows = self.storage.query_history(&received_q).await?;

        let pending = self.storage.list_pending(identity).await?.len() as u64;

        Ok(ActivityStats {
            identity: identity.to_string(),
            sent: sent_rows.len() as u64,
            received_dm: received_rows.len() as u64,
            events,
            pending,
        })
    }

    async fn channel_hotspots(&self, range: &TimeRange, limit: usize) -> Result<Vec<ChannelStats>> {
        let rows = self
            .storage
            .query_history(&Self::range_query(range))
            .await?;

        let mut counts: HashMap<String, u64> = HashMap::new();
        for row in &rows {
            *counts.entry(row.channel.clone()).or_insert(0) += 1;
        }

        let mut stats: Vec<ChannelStats> = counts
            .into_iter()
            .map(|(channel, messages)| ChannelStats { channel, messages })
            .collect();
        stats.sort_by(|a, b| {
            b.messages
                .cmp(&a.messages)
                .then_with(|| a.channel.cmp(&b.channel))
        });
        stats.truncate(limit);

        Ok(stats)
    }

    async fn error_rate(&self, range: &TimeRange, group_by: Interval) -> Result<Vec<DataPoint>> {
        let rows = self
            .storage
            .query_history(&Self::range_query(range))
            .await?;

        let errors: Vec<crate::storage::EnvelopeRecord> = rows
            .into_iter()
            .filter(|r| {
                r.kind == "event"
                    && r.payload.get("event_type").and_then(|v| v.as_str()) == Some("error")
            })
            .collect();

        Ok(Self::bucketize(&errors, range, group_by))
    }
}
