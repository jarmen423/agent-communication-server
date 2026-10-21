//! Analytics trait + query types for nats-hub observability.
//!
//! The `Analytics` trait is a read-side peer to the [`Storage`](crate::storage::Storage)
//! trait. It answers operational questions (message rates, channel hotspots,
//! agent activity, reply latency, error rate) by querying the already-persisted
//! `envelopes` history. It does NOT write to the database and does NOT require
//! any schema change — every signal is already in `EnvelopeRecord`.
//!
//! See `docs/archive/PHASE4_PLAN.md` (sub-phase 4a) for the design rationale.

use anyhow::Result;
use async_trait::async_trait;

#[cfg(feature = "storage-surreal")]
mod surreal;

#[cfg(feature = "storage-surreal")]
pub use surreal::SurrealAnalytics;

/// Live, in-memory metrics collector (Prometheus-compatible exposition).
/// Always compiled — independent of the storage feature so
/// `hub-server --metrics-addr` works in `--features no-storage` mode.
pub mod metrics;

pub use nats_hub_client::analytics::*;

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
