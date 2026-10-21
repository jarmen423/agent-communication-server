//! Analytics query parameters and result records.

use chrono::{DateTime, Utc};

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
