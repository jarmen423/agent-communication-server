//! Analytics trait + query types for nats-hub observability.
//!
//! The `Analytics` trait is a read-side peer to the [`Storage`](crate::storage::Storage)
//! trait. It answers operational questions (message rates, channel hotspots,
//! agent activity, reply latency, error rate) by querying the already-persisted
//! `envelopes` history. It does NOT write to the database and does NOT require
//! any schema change — every signal is already in `EnvelopeRecord`.
//!
//! See `docs/PHASE4_PLAN.md` (sub-phase 4a) for the design rationale.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[cfg(feature = "storage-surreal")]
mod surreal;

#[cfg(feature = "storage-surreal")]
pub use surreal::SurrealAnalytics;

/// Live, in-memory metrics collector (Prometheus-compatible exposition).
/// Always compiled — independent of the storage feature so
/// `hub-server --metrics-addr` works in `--features no-storage` mode.
pub mod metrics;

/// A time window for analytics queries.
#[derive(Debug, Clone)]
pub struct TimeRange {
    /// Lower bound (inclusive).
    pub since: DateTime<Utc>,
    /// Upper bound (inclusive). `None` means "up to now".
    pub until: Option<DateTime<Utc>>,
}

impl TimeRange {
    /// Window covering the last `secs` seconds (until now).
    pub fn last(secs: i64) -> Self {
        Self {
            since: Utc::now() - chrono::Duration::seconds(secs),
            until: None,
        }
    }

    /// Window starting at `t` and open-ended (until now).
    pub fn since(t: DateTime<Utc>) -> Self {
        Self {
            since: t,
            until: None,
        }
    }

    /// Fully-bounded window `[since, until]`.
    pub fn bounded(since: DateTime<Utc>, until: DateTime<Utc>) -> Self {
        Self {
            since,
            until: Some(until),
        }
    }
}

/// Bucketing granularity for time-series methods.
#[derive(Debug, Clone, Copy)]
pub enum Interval {
    Minute,
    Hour,
    Day,
}

impl Interval {
    /// Number of seconds in one bucket of this interval.
    pub fn as_secs(&self) -> i64 {
        match self {
            Interval::Minute => 60,
            Interval::Hour => 3600,
            Interval::Day => 86_400,
        }
    }
}

/// A single `(bucket_start, count)` point in a time series.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DataPoint {
    pub timestamp: DateTime<Utc>,
    pub count: u64,
}

/// Latency distribution for answered (reply) messages.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LatencyStats {
    pub samples: u64,
    pub avg_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
}

/// Per-agent activity summary over a time range.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActivityStats {
    pub identity: String,
    pub sent: u64,
    pub received_dm: u64,
    pub events: u64,
    pub pending: u64,
}

/// Per-channel volume row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChannelStats {
    pub channel: String,
    pub messages: u64,
}

/// Read-side analytics backend for nats-hub.
///
/// Implementations derive metrics from persisted history. The default
/// implementation is [`SurrealAnalytics`] (feature = `storage-surreal`).
#[async_trait]
pub trait Analytics: Send + Sync {
    /// Message count over a time range, bucketed by `group_by`.
    async fn message_rate(&self, range: &TimeRange, group_by: Interval) -> Result<Vec<DataPoint>>;

    /// Reply-latency distribution (round-trip time for answered messages)
    /// over a time range, optionally scoped to one channel.
    async fn latency_stats(&self, channel: Option<&str>, range: &TimeRange)
        -> Result<LatencyStats>;

    /// Per-agent activity (sent, received DM, events, pending).
    async fn agent_activity(&self, identity: &str, range: &TimeRange) -> Result<ActivityStats>;

    /// Top channels by message volume in a range.
    async fn channel_hotspots(&self, range: &TimeRange, limit: usize) -> Result<Vec<ChannelStats>>;

    /// Error-event rate over a time range, bucketed by `group_by`.
    /// Counts envelopes where `kind == "event"` AND `payload.event_type == "error"`.
    async fn error_rate(&self, range: &TimeRange, group_by: Interval) -> Result<Vec<DataPoint>>;
}
