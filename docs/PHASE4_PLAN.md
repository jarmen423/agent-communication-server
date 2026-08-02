# Phase 4: Analytics Trait + Observability

## Status

| Sub-phase | Status | Deliverable |
|---|---|---|
| **4a** Analytics trait + `SurrealAnalytics` + `hub-stats` | ✅ Done | `src/analytics/`, `hub-stats` CLI |
| **4b** Prometheus metrics exporter | ✅ Done | `MetricsCollector`, `hub-server --metrics-addr` |
| **4c** DuckDB OLAP backend (optional) | 📋 Deferred | `DuckdbAnalytics` behind `analytics-duckdb` — trait is backend-agnostic; add when heavy analytical workloads appear |

**Verification target:** `CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats cargo test` — all existing + new tests pass; `cargo build` clean.

## Overview

Phase 4 turns nats-hub from a *persisting* bus into an *observable* bus. Phases 1–3 built the plumbing (transport → router → SurrealDB mirror) and the orchestration layer (sessions, events, waves). All of that already writes a complete, queryable history of every envelope. Phase 4 adds the read-side analytics that answer operational questions:

- **How busy is the bus?** message rate over time, per interval
- **Which channels are hot?** top channels by volume
- **Who's doing what?** per-agent send/receive/event/pending activity
- **How fast do agents respond?** reply latency (round-trip time for answered messages)
- **Is anything failing?** error-event rate over time
- **Live metrics** for dashboards/Prometheus scrapes (real-time, not DB-bound)

This phase is **read-only against the existing `envelopes` table**. No new schema fields are required for 4a/4b — every signal is already in `EnvelopeRecord` (`timestamp`, `from_identity`, `to_identity`, `channel`, `kind`, `reply_to`, `payload.event_type`). This keeps Phase 4 low-risk: it adds a new `Analytics` trait (peer to `Storage`) and a CLI, without touching the hot path or migration.

**Inspired by:** the `Analytics` trait sketch in `docs/DATABASE_PLAN.md` §Analytics Trait, and the observability best-practice from the project PR closeout checklist (human-facing endpoint **and** Prometheus metrics, bounded low-cardinality labels).

---

## What data we already have (no schema change needed)

`EnvelopeRecord` (stored by `store_envelope`) carries everything 4a needs:

| Field | Used by | Notes |
|---|---|---|
| `timestamp` | all | envelope creation time (UTC) |
| `from_identity` | agent_activity, message_rate-by-agent | sender |
| `to_identity` | agent_activity (received), latency | DM recipient; `None` = broadcast |
| `channel` | channel_hotspots, latency, message_rate | e.g. `wave.w1.task.t1`, `inbox.worker-1`, `agents.broadcast` |
| `kind` | error_rate, message_rate | `"message" \| "control" \| "human" \| "status" \| "event"` (lowercased debug) |
| `reply_to` | latency | correlation ID → original message id (when set) |
| `payload.event_type` | error_rate | only present when `kind == "event"`; one of `started/progress/stdout/milestone/completed/error` |

Derivable metrics:
- **message_rate / channel_hotspots / error_rate**: single `query_history` over a `TimeRange`, then group in Rust (no native GROUP BY needed — matches the existing "fetch then filter in Rust" pattern used across `surreal.rs`).
- **agent_activity**: `query_history(from = agent)` for sent + events; `list_pending(agent)` for pending; `query_history(to = agent)` for received DMs.
- **latency_stats**: one range `query_history` → build `HashMap<id, timestamp>` → for every reply (`reply_to` set) look up the original → delta. Single query, in-memory map, no per-reply DB round-trips.

---

## Sub-phase 4a: Analytics trait + `SurrealAnalytics` + `hub-stats`

> Build the `Analytics` trait (peer to `Storage`), a `SurrealAnalytics` implementation, and the `hub-stats` CLI.

### New types (`src/analytics/mod.rs`)

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A time window for analytics queries.
#[derive(Debug, Clone)]
pub struct TimeRange {
    pub since: DateTime<Utc>,
    /// Inclusive upper bound. Defaults to `now` when `None`.
    pub until: Option<DateTime<Utc>>,
}

impl TimeRange {
    /// Last `secs` seconds.
    pub fn last(secs: i64) -> Self {
        Self { since: Utc::now() - chrono::Duration::seconds(secs), until: None }
    }
    pub fn since(t: DateTime<Utc>) -> Self { Self { since: t, until: None } }
    pub fn bounded(since: DateTime<Utc>, until: DateTime<Utc>) -> Self {
        Self { since, until: Some(until) }
    }
    fn until_or_now(&self) -> DateTime<Utc> { self.until.unwrap_or_else(Utc::now) }
}

/// Bucketing granularity for time-series methods.
#[derive(Debug, Clone, Copy)]
pub enum Interval { Minute, Hour, Day }

impl Interval {
    /// Seconds in this interval (used for bucket math).
    pub fn as_secs(&self) -> i64 {
        match self { Interval::Minute => 60, Interval::Hour => 3600, Interval::Day => 86_400 }
    }
}

/// A single (bucket_start, count) point in a time series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataPoint {
    pub timestamp: DateTime<Utc>,
    pub count: u64,
}

/// Latency distribution for answered messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyStats {
    pub samples: u64,
    pub avg_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
}

/// Per-agent activity summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityStats {
    pub identity: String,
    pub sent: u64,
    pub received_dm: u64,
    pub events: u64,
    pub pending: u64,
}

/// Per-channel volume row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelStats {
    pub channel: String,
    pub messages: u64,
}
```

### `Analytics` trait (`src/analytics/mod.rs`)

```rust
#[async_trait]
pub trait Analytics: Send + Sync {
    /// Message count over a time range, bucketed by `group_by`.
    async fn message_rate(&self, range: &TimeRange, group_by: Interval) -> Result<Vec<DataPoint>>;

    /// Reply-latency distribution (round-trip time for answered messages)
    /// over a time range, optionally scoped to one channel.
    async fn latency_stats(&self, channel: Option<&str>, range: &TimeRange) -> Result<LatencyStats>;

    /// Per-agent activity (sent, received DM, events, pending).
    async fn agent_activity(&self, identity: &str, range: &TimeRange) -> Result<ActivityStats>;

    /// Top channels by message volume in a range.
    async fn channel_hotspots(&self, range: &TimeRange, limit: usize) -> Result<Vec<ChannelStats>>;

    /// Error-event rate over a time range, bucketed by `group_by`.
    /// Counts envelopes where kind == "event" AND payload.event_type == "error".
    async fn error_rate(&self, range: &TimeRange, group_by: Interval) -> Result<Vec<DataPoint>>;
}
```

### `SurrealAnalytics` impl (`src/analytics/surreal.rs`)

Wraps `Arc<SurrealStorage>` (or any `Storage`) and reuses existing queries — it does **not** add new SurrealQL paths beyond what `query_history`/`list_pending`/`get_envelope` already provide.

```rust
pub struct SurrealAnalytics {
    storage: Arc<dyn Storage>,
}

impl SurrealAnalytics {
    pub fn new(storage: Arc<dyn Storage>) -> Self { Self { storage } }
}
```

Implementation approach per method (all operate on a single range `query_history` pull, then aggregate in Rust — consistent with the existing pattern):

- **`message_rate`**: `query_history(since, until)`, bucket by `floor((ts - range.since).as_secs() / interval.as_secs())`, count per bucket, emit `Vec<DataPoint>` (zero-filled gaps optional; emit only non-empty buckets + always emit the first/last boundary for chart continuity).
- **`latency_stats`**: range pull → `HashMap<id, timestamp>`. For each env where `reply_to.is_some()`: `latency_ms = (env.timestamp - map[reply_to]).as_millis()`. Collect, sort, compute avg/min/max/p50/p99. If `channel` filter given, only consider envelopes on that channel (both original and reply must be in the pulled set; since we pull by range not channel, filter replies by channel and look up originals by id from the same set).
- **`agent_activity`**: 
  - `sent` = `query_history(from = identity, since/until).len()`
  - `events` = same, filtered `kind == "event"`
  - `received_dm` = `query_history(to = identity, since/until).len()`
  - `pending` = `list_pending(identity).len()` (time-unbounded pending count is fine; optionally filter by `since` if needed)
- **`channel_hotspots`**: range pull → `HashMap<channel, count>` → sort desc → take `limit`.
- **`error_rate`**: range pull → filter `kind == "event"` and `payload.event_type == "error"` → bucket like `message_rate`.

> **Scale note:** all methods pull the range once and aggregate in Rust. This matches the project's "DB is a sidecar, modest scale, fetch-then-filter" convention (see `surreal.rs` `find_agents`/`list_pending`). A future optimization can push `count()` + `GROUP BY time::bucket(...)` into SurrealQL, but it is **out of scope** for 4a (keeps the impl robust against SurrealDB version specifics).

### `HistoryQuery` builder additions (`src/storage/mod.rs`)

Add two builders so analytics can express ranges cleanly (currently `since` exists but `until`/`kind` are field-only):

```rust
impl HistoryQuery {
    pub fn until(mut self, ts: DateTime<Utc>) -> Self { self.until = Some(ts); self }
    pub fn kind(mut self, k: impl Into<String>) -> Self { self.kind = Some(k.into()); self }
}
```

No schema/migration change.

### New CLI: `hub-stats` (`src/bin/hub_stats.rs`)

```text
hub-stats --db-path nats_hub.db --since 1h
    # prints: total messages, message_rate buckets, top channels, error rate

hub-stats --db-path nats_hub.db --agent worker-1 --since 24h
    # prints: agent_activity (sent/received/events/pending)

hub-stats --db-path nats_hub.db --latency --channel agents.tasks --since 6h
    # prints: LatencyStats (avg/p50/p99...)

hub-stats --db-path nats_hub.db --top-channels 10 --since 1h

hub-stats --db-path nats_hub.db --json
    # emit a single JSON object with all sections (for dashboards/pipes)
```

Design:
- Connects to `SurrealStorage`, `migrate()`, builds `SurrealAnalytics`.
- `--since <dur>` parses `30m | 6h | 7d` into `TimeRange::last(...)`. Default `1h`.
- Human-readable tables by default; `--json` emits one `serde_json::Value` blob.
- Pretty-print helpers mirror `hub_history.rs` formatting (reuse `truncate_str`/`preview_payload` — extract to a shared `src/bin/common.rs` or just duplicate; prefer a tiny `src/bin/stats_print.rs` module if it grows past ~60 LOC).

### File Impact (4a)

| File | Change | Est. LOC |
|---|---|---|
| `src/analytics/mod.rs` | `Analytics` trait + types (`TimeRange`, `Interval`, `DataPoint`, `LatencyStats`, `ActivityStats`, `ChannelStats`) | ~160 |
| `src/analytics/surreal.rs` | `SurrealAnalytics` impl (5 methods) | ~180 |
| `src/storage/mod.rs` | `HistoryQuery::until` / `kind` builders | ~8 |
| `src/lib.rs` | re-export `analytics::{Analytics, …}` under `storage-surreal` | ~3 |
| `src/bin/hub_stats.rs` | New CLI binary | ~220 |
| `Cargo.toml` | `[[bin]] hub-stats` (required-features = storage-surreal) | ~3 |
| `tests/analytics.rs` | 5–6 tests | ~160 |
| **Total** | | **~735** |

### Tests (`tests/analytics.rs`)

Use `SurrealStorage::connect_memory()` + `migrate()` (same harness as `tests/waves.rs`). Seed envelopes with controlled `timestamp`s via `store_envelope` on crafted `Envelope`s (set `meta.timestamp` manually — `Envelope::new` uses `Utc::now()`, so construct `Envelope { meta: Meta { timestamp: ..., .. }, .. }` directly or add a `with_timestamp` test helper).

1. `test_message_rate_buckets` — seed 10 envelopes across 2 minutes; `message_rate(Minute)` returns 2 buckets summing to 10.
2. `test_channel_hotspots` — seed across 3 channels; top-1 returns the busiest.
3. `test_agent_activity` — seed sent/received/event/pending for `agent-x`; assert counts; seed a pending DM via `to_identity` and verify `list_pending` contributes.
4. `test_latency_stats` — seed an original + a reply (`reply_to = original.id`, later timestamp); assert `avg_ms` ≈ delta, `samples == 1`.
5. `test_error_rate` — seed 3 `event`/`error` envelopes + 2 normal events; `error_rate` counts only the 3 errors.
6. `test_time_range_filter` — `TimeRange::bounded` excludes out-of-window envelopes.

---

## Sub-phase 4b: Prometheus metrics exporter

> Live, real-time observability without DB reads. Adds a `MetricsCollector` (atomic counters, bounded labels) wired into the router's hot path, plus a `/metrics` HTTP endpoint on `hub-server`.

**Why both 4a and 4b:** 4a answers *historical* questions (cold path, DB). 4b answers *live* questions (real-time, in-memory) for Prometheus/Grafana. This is the observability best-practice split: a human-facing CLI (4a) **and** a metrics endpoint (4b), with **bounded low-cardinality labels** (per the project PR closeout checklist).

### `MetricsCollector` (`src/analytics/metrics.rs`)

A zero-dependency collector using `std::sync::atomic` (no `prometheus` crate needed — we hand-render the exposition text, keeping deps minimal):

```rust
#[derive(Default)]
pub struct MetricsCollector {
    // Bounded labels only:
    pub messages_total: AtomicU64,                       // grand total
    pub by_kind: [AtomicU64; 5],                         // message/control/human/status/event
    pub by_channel_class: [AtomicU64; 6],                // broadcast/inbox/session/wave/task/other
    pub errors_total: AtomicU64,                         // event_type == "error"
    pub latency_sum_ms: AtomicU64,                       // summed reply latency (for avg)
    pub latency_samples: AtomicU64,
}
```

- **`record(&self, env: &Envelope)`** — called once per routed envelope in `ControlPlane::handle_send`:
  - `messages_total += 1`
  - increment `by_kind[kind_index(env.meta.kind)]`
  - classify `channel` into a class (see `channel_class()` below) → increment `by_channel_class`
  - if `kind == Event` and `event_type == "error"` → `errors_total += 1`
  - if `reply_to.is_some()` → best-effort latency: we don't have the original here (router only sees the reply), so latency is **not** tracked in the hot path. Instead, expose `latency` via 4a's DB query. (Keeps the hot path a pure counter increment — sub-microsecond.)
- **`channel_class(channel: &str) -> ChannelClass`** — bounded bucketing:
  - starts with `inbox.` → `Inbox`
  - starts with `session.` → `Session`
  - starts with `wave.` → `Wave`
  - starts with `task.` → `Task`
  - otherwise → `Broadcast` (e.g. `agents.broadcast`) or `Other`
  This bounds the `channel_class` label to **6 values** regardless of bus size.
- **`render_prometheus(&self) -> String`** — emits text exposition:
  ```text
  # HELP natshub_messages_total Total envelopes routed
  # TYPE natshub_messages_total counter
  natshub_messages_total 1234
  # HELP natshub_messages_by_kind ...
  natshub_messages_by_kind{kind="event"} 42
  ...
  # HELP natshub_messages_by_channel_class ...
  natshub_messages_by_channel_class{class="inbox"} 7
  # HELP natshub_errors_total Total error events
  # TYPE natshub_errors_total counter
  natshub_errors_total 3
  ```
  Per-agent counters are **intentionally excluded** from the default metrics (high cardinality). If needed later, gate behind a flag and limit to top-N by an out-of-band snapshot.

### Wire into the router (`src/router.rs`)

- Add `metrics: Option<Arc<MetricsCollector>>` to `ControlPlane`.
- `pub fn with_metrics(mut self, m: Arc<MetricsCollector>) -> Self` (mirrors `with_storage`).
- In `handle_send`, after routing (or before — order doesn't matter, it's fire-and-forget atomic), if metrics attached: `metrics.record(&env)`.
- This is a **non-blocking atomic increment** — no allocation, no await, safe on the hot path.

### `hub-server --metrics-addr` (`src/bin/hub_server.rs`)

- New flag `--metrics-addr` (default: none / disabled). When set (e.g. `127.0.0.1:9090`), `hub-server` spawns a tiny HTTP listener **in a background task** that responds only to `GET /metrics` with `text/plain; version=0.0.4` body from `MetricsCollector::render_prometheus`.
- **Zero new web dependencies:** use `std::net::TcpListener` + a minimal HTTP/1.0 responder (accept → read request line → if `GET /metrics` respond with metrics, else 404). ~40 LOC. No `hyper`/`axum` needed.
- The listener task clones the `Arc<MetricsCollector>` so it reads live counters.
- Works in **both** `storage-surreal` and `no-storage` modes (metrics are in-memory, independent of persistence).

### File Impact (4b)

| File | Change | Est. LOC |
|---|---|---|
| `src/analytics/metrics.rs` | `MetricsCollector`, `ChannelClass`, `record`, `render_prometheus` | ~150 |
| `src/router.rs` | `metrics` field, `with_metrics`, `record` call in `handle_send` | ~20 |
| `src/bin/hub_server.rs` | `--metrics-addr` flag + listener task | ~60 |
| `src/lib.rs` | re-export `analytics::MetricsCollector` (always, even no-storage) | ~2 |
| `tests/metrics_tests.rs` | unit tests for `MetricsCollector` (record + render) | ~90 |
| **Total** | | **~322** |

### Tests (4b)

1. `test_record_increments` — `record` on a message + event/error envelope; assert `messages_total`, `by_kind`, `errors_total`.
2. `test_channel_class` — `channel_class("inbox.worker-1") == Inbox`, `("wave.w1.task.t1") == Wave`, `("agents.broadcast") == Broadcast`.
3. `test_render_prometheus` — render output contains expected metric names + a counter value; asserts bounded label values only.
4. `test_metrics_survives_no_storage` — construct `ControlPlane` without storage but with metrics; route a fake envelope (unit-style, no NATS) — verify counters increment. (May use a small direct `record` call rather than full NATS round-trip.)

---

## Sub-phase 4c (optional / stretch): DuckDB OLAP backend

> Only if heavy analytical workloads appear. Kept optional per `DATABASE_PLAN.md`.

- Add `analytics-duckdb` feature → `dep:duckdb` (bundled).
- `DuckdbAnalytics` impl of the same `Analytics` trait, reading from a Parquet/Arrow export of `envelopes` (DuckDB cannot read SurrealDB's RocksDB directly — data must be exported via the `Analytics`/`Storage` path first).
- Provides the same 5 methods using native SQL `COUNT`/`GROUP BY`/`PERCENTILE` for scale.
- **Out of scope for the initial Phase 4 landing.** Documented here so the trait is designed to accommodate it (it already is — `Analytics` is backend-agnostic).

---

## Cross-cutting concerns

### Feature flags
- `src/analytics/mod.rs` + `src/analytics/surreal.rs` are gated under `storage-surreal` (they need `SurrealStorage`/`Storage`).
- `src/analytics/metrics.rs` + `MetricsCollector` are **always compiled** (no storage dependency) so `hub-server --metrics-addr` works even in `no-storage` mode.
- `hub-stats` binary: `required-features = ["storage-surreal"]`.
- Add to `Cargo.toml`:
  ```toml
  [[bin]]
  name = "hub-stats"
  path = "src/bin/hub_stats.rs"
  required-features = ["storage-surreal"]
  ```
- `Analytics` trait + `SurrealAnalytics` re-exported from `lib.rs` under `#[cfg(feature = "storage-surreal")]`. `MetricsCollector` re-exported unconditionally.

### No schema migration
4a/4b require **zero** `DEFINE TABLE`/`INDEX` changes. All signals exist in `envelopes`. This is deliberate — keeps Phase 4 decoupled from `migrate()` and from any running-DB compatibility concerns.

### Hardening / conventions (match existing code)
- All new I/O async (`tokio`); `MetricsCollector` is sync atomics (no await).
- Files stay under ~400 LOC — `analytics/surreal.rs` splits into `mod.rs` (trait+types) + `surreal.rs` (impl) as above; `metrics.rs` is its own module.
- Tests that need NATS skip gracefully (none of 4a/4b strictly need a live NATS server — they use `connect_memory` / direct `record` calls).
- Set `CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats` for all builds.

---

## Implementation Order

1. **4a first** — the `Analytics` trait + `SurrealAnalytics` + `hub-stats`. This is the core deliverable and the one referenced by `PRODUCT_VISION.md` / `DATABASE_PLAN.md`. It is pure read-side and safe.
2. **4b second** — `MetricsCollector` + router wiring + `hub-server --metrics-addr`. Builds on the same `Envelope` shape; independent of 4a's DB queries.
3. **4c last (optional)** — only if a real OLAP need emerges.

---

## Verification

After each sub-phase (and at the end):

1. `CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats cargo test` — all tests pass (existing 39 + new `analytics`/`metrics` tests).
2. `CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats cargo build` — clean, no warnings. Also verify `cargo build --no-default-features --features no-storage` still compiles (metrics path must not pull storage).
3. **Manual dogfood (4a):** start NATS + `hub-server` + a worker, generate some traffic, run `hub-stats --since 1h` and `--json`; confirm sane numbers; run `hub-stats --agent <worker>`.
4. **Manual dogfood (4b):** start `hub-server --metrics-addr 127.0.0.1:9090`, generate traffic, `curl localhost:9090/metrics` → confirm exposition format + bounded labels; confirm it works with `--features no-storage`.
5. Update `docs/DATABASE_PLAN.md` (check off Phase 4 items), `docs/PRODUCT_VISION.md` (mark Phase 4 status + add `analytics/` to file layout), and `AGENTS.md` (add `src/analytics/` to code layout + `hub-stats` to CLI reference).

---

## Open questions / design notes

- **Latency definition.** We define `latency_stats` as *reply round-trip time* (reply.timestamp − original.timestamp) because it's directly derivable from `reply_to` + `timestamp` and is the most operationally useful "how fast do agents respond" signal. An alternative — *worker processing time* (event `completed` − event `started`) — is also derivable but requires correlating events within a session/task and is left as a future enhancement. The hot-path `MetricsCollector` deliberately does **not** compute latency (it only has the reply, not the original) — latency stays a DB/4a concern.
- **Bucket gaps.** `message_rate` emits only non-empty buckets by default. If a dashboard needs continuous series, we can zero-fill gaps in a follow-up; not required for the CLI.
- **`agent_activity::pending`** uses `list_pending` which is time-unbounded. If a `--since` filter should also bound pending, we can post-filter by `timestamp >= range.since` (cheap, in Rust).
- **Per-agent Prometheus metrics** are excluded by default (cardinality). If Josh wants per-agent counters, recommend an out-of-band top-N snapshot rather than a label per agent.
