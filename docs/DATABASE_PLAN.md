# nats-hub — Database & Persistence Plan

## Goal

Add a pluggable persistence layer to nats-hub so it serves as a portable,
drop-in communication dependency for any Rust project — especially agent-based
systems — that provides queryable message history, agent registry, conversation
threading, and observability out of the box.

## Design Principles

1. **NATS moves messages. The DB answers questions about messages.** Transport
   and persistence are separate concerns.
2. **CQRS by default.** The hot path (agent → NATS → router → subscribers) never
   touches the DB. An async mirror off the router populates the DB for queries.
3. **Trait-based abstraction.** `Storage` and `Analytics` traits let downstream
   projects pick their engine via Cargo feature flags.
4. **Embedded by default.** Zero-config for the common case. No server required.
5. **No hot-path DB writes.** The DB is a read/query sidecar, not a write gate.
6. **Host project owns its application data.** nats-hub's DB stores messaging
   metadata, agent state, and observability data — not agent memory or
   application state.
7. **SurrealDB stays behind the Storage trait.** Third parties never get direct
   database access. This keeps us clear of the BSL DBaaS restriction even when
   hosting nats-hub as a service.

## Database Selection (decided June 2026)

### Default: SurrealDB (multi-model, graph-native, embedded)

- **Why**: Native graph traversal — conversation threading is a graph problem,
  and recursive CTEs in SQL are painful. Document model fits free-form JSON
  payloads natively. Vector search built in for future semantic queries.
  Pure Rust engine, same as the rest of the codebase. Single binary, embedded.
- **Crate**: `surrealdb` with `kv-rocksdb` feature (embedded RocksDB storage).
- **Maturity**: v3.1.5, production-used at scale. 32K GitHub stars.
- **License**: Core engine is BSL 1.1 (converts to Apache 2.0 on 2030-01-01).
  Rust SDK is Apache 2.0. BSL restriction only blocks offering SurrealDB as a
  DBaaS to third parties — nats-hub provides messaging, not database access,
  so this is a non-issue. See `docs/SURREALDB_LICENSE.md` for details.
- **Trade-off**: SurrealQL is non-standard. Mitigated by the `Storage` trait —
  downstream projects never see SurrealQL.

### Optional: PostgreSQL + pgvector (server-based, scale)

- **Why**: Rock-solid, multi-node, high-concurrency. pgvector for semantic
  search. Everyone knows SQL.
- **When to choose**: Multi-node deployments, high write concurrency, or when
  the host project already runs Postgres.
- **Crate**: `sqlx` with `postgres` feature.

### Optional: libSQL (SQLite-compatible, embedded)

- **Why**: Maximum portability, zero dependencies, every tool reads the file.
- **When to choose**: Extremely constrained environments, or when SQLite
  compatibility is required by the host project.
- **Crate**: `libsql` with `core` feature.

### Analytics: SurrealDB native (default) or DuckDB (optional)

- **SurrealDB native**: For most observability queries (message rates, agent
  activity, channel hotspots), SurrealDB's built-in aggregations and time-series
  support are sufficient. No separate analytics engine needed.
- **DuckDB** (optional): For heavy OLAP workloads, DuckDB can export data via
  Arrow/Parquet and run columnar analytics. Note: DuckDB can read SQLite files
  directly but cannot read SurrealDB's RocksDB store — data would need to be
  exported via the Analytics trait.
- **Crate**: `duckdb` with `bundled` feature (optional).

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                    Host Project                          │
│  (brings its own agents, its own app DB if needed)       │
├──────────────────────────────────────────────────────────┤
│                      nats-hub crate                      │
│                                                          │
│  ┌─────────────┐  ┌──────────────┐  ┌────────────────┐ │
│  │  Transport   │  │  Storage      │  │  Analytics     │ │
│  │  (NATS)      │  │  (trait)      │  │  (trait)       │ │
│  │  pub/sub     │  │               │  │                │ │
│  │  routing     │  │  ┌──────────┐ │  │  ┌──────────┐ │ │
│  │  request/    │  │  │SurrealDB │ │  │  │SurrealDB │ │ │
│  │  reply       │  │  │ (default)│ │  │  │ (default)│ │ │
│  │              │  │  ├──────────┤ │  │  ├──────────┤ │ │
│  │              │  │  │ Postgres │ │  │  │ DuckDB   │ │ │
│  │              │  │  │ libSQL   │ │  │  │          │ │ │
│  │              │  │  └──────────┘ │  │  └──────────┘ │ │
│  └─────────────┘  └──────────────┘  └────────────────┘ │
│                                                          │
│  ┌─────────────────────────────────────────────────────┐ │
│  │  Control Plane Router                                │ │
│  │  hub.send.> → channel.<name>  (real-time, in-memory) │ │
│  │       │                                              │ │
│  │       └──→ async mirror → Storage::store_envelope()  │ │
│  └─────────────────────────────────────────────────────┘ │
│                                                          │
│  ┌─────────────────────────────────────────────────────┐ │
│  │  BSL Safeguard: SurrealDB is always behind the       │ │
│  │  Storage trait. Third parties interact with nats-hub │ │
│  │  messaging API, never with SurrealDB directly.       │ │
│  └─────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────┘
```

### Data flow

1. **Hot path (real-time delivery)**: Agent publishes to `hub.send.<channel>` →
   router re-publishes to `channel.<name>` → subscribers receive. No DB I/O.
   Sub-millisecond latency.

2. **Async mirror (write to DB)**: The router (or a dedicated worker) fire-and-
   forgets each envelope to `Storage::store_envelope()`. This is off the hot
   path — if the DB write fails, a warning is logged but delivery is unaffected.

3. **Query path (read from DB)**: `query_history()`, `find_agents()`,
   `get_thread()` etc. hit the DB. These are latency-tolerant operations
   for debugging, audit, and observability.

## Feature Flags

```toml
[features]
default = ["storage-surreal"]
storage-surreal = ["dep:surrealdb"]
storage-postgres = ["dep:sqlx"]
storage-libsql = ["dep:libsql"]
analytics-duckdb = ["dep:duckdb"]
```

Only one `storage-*` feature should be enabled at a time. Analytics features are
independent and optional.

## Storage Trait

```rust
#[async_trait]
pub trait Storage: Send + Sync {
    // ── Agent Registry ──────────────────────────────────────
    /// Register or update an agent. Persisted across restarts.
    async fn register_agent(&self, agent: AgentRecord) -> Result<()>;

    /// Remove an agent from the registry.
    async fn deregister_agent(&self, identity: &str) -> Result<()>;

    /// Update agent liveness (called on heartbeat).
    async fn touch_agent(&self, identity: &str) -> Result<()>;

    /// Find agents matching a filter (capabilities, liveness, etc.).
    async fn find_agents(&self, filter: AgentFilter) -> Result<Vec<AgentRecord>>;

    /// Get a single agent by identity.
    async fn get_agent(&self, identity: &str) -> Result<Option<AgentRecord>>;

    // ── Message History ─────────────────────────────────────
    /// Store an envelope (called by the async mirror off the router).
    async fn store_envelope(&self, env: &Envelope) -> Result<()>;

    /// Query message history with filters.
    async fn query_history(&self, q: &HistoryQuery) -> Result<Vec<EnvelopeRecord>>;

    /// Get a single envelope by ID.
    async fn get_envelope(&self, id: &str) -> Result<Option<EnvelopeRecord>>;

    // ── Conversation Threading (graph) ──────────────────────
    /// Record a reply relationship (envelope A is a reply to envelope B).
    async fn link_reply(&self, reply_id: &str, parent_id: &str) -> Result<()>;

    /// Get a conversation thread starting from a root message.
    /// Uses SurrealDB graph traversal: SELECT ->reply_to->envelope.* ...
    async fn get_thread(&self, root_id: &str) -> Result<Vec<EnvelopeRecord>>;

    /// List pending (unanswered) messages for an agent.
    async fn list_pending(&self, identity: &str) -> Result<Vec<EnvelopeRecord>>;

    // ── Lifecycle ───────────────────────────────────────────
    /// Initialize the schema (create tables, indexes, etc.).
    async fn migrate(&self) -> Result<()>;

    /// Health check.
    async fn ping(&self) -> Result<()>;
}
```

## Analytics Trait

```rust
#[async_trait]
pub trait Analytics: Send + Sync {
    /// Message count over a time range, grouped by interval.
    async fn message_rate(&self, range: TimeRange, group_by: Interval) -> Result<Vec<DataPoint>>;

    /// Average/min/max/p50/p99 delivery latency per channel.
    async fn latency_stats(&self, channel: &str, range: TimeRange) -> Result<LatencyStats>;

    /// Agent activity summary (messages sent, received, pending).
    async fn agent_activity(&self, identity: &str, range: TimeRange) -> Result<ActivityStats>;

    /// Channel hotspots (top channels by volume).
    async fn channel_hotspots(&self, range: TimeRange, limit: usize) -> Result<Vec<ChannelStats>>;

    /// Error rate over time.
    async fn error_rate(&self, range: TimeRange, group_by: Interval) -> Result<Vec<DataPoint>>;
}
```

## Schema (SurrealQL default)

```surql
-- Namespace and database
DEFINE NAMESPACE nats_hub;
DEFINE DATABASE messaging;

-- Agent registry (schemaless document table)
DEFINE TABLE agents SCHEMALESS;
DEFINE FIELD identity       AT agents TYPE string;
DEFINE FIELD capabilities   AT agents TYPE array<string>;
DEFINE FIELD last_seen      AT agents TYPE datetime;
DEFINE FIELD registered_at  AT agents TYPE datetime;
DEFINE FIELD metadata       AT agents TYPE object;

DEFINE INDEX idx_agents_last_seen ON TABLE agents COLUMNS last_seen;

-- Envelope history (schemaless document table)
DEFINE TABLE envelopes SCHEMALESS;
DEFINE FIELD id            AT envelopes TYPE string;
DEFINE FIELD from_identity AT envelopes TYPE string;
DEFINE FIELD channel       AT envelopes TYPE string;
DEFINE FIELD to_identity   AT envelopes TYPE option<string>;
DEFINE FIELD timestamp     AT envelopes TYPE datetime;
DEFINE FIELD kind          AT envelopes TYPE string;
DEFINE FIELD reply_to      AT envelopes TYPE option<string>;
DEFINE FIELD payload       AT envelopes TYPE object;
DEFINE FIELD stored_at     AT envelopes TYPE datetime;

DEFINE INDEX idx_env_channel_time ON TABLE envelopes COLUMNS channel, timestamp;
DEFINE INDEX idx_env_from_time    ON TABLE envelopes COLUMNS from_identity, timestamp;
DEFINE INDEX idx_env_kind_time    ON TABLE envelopes COLUMNS kind, timestamp;

-- Conversation threading (graph edges)
-- reply_to is a relation FROM a reply envelope TO its parent envelope
DEFINE TABLE reply_to SCHEMALESS TYPE RELATION FROM envelopes TO envelopes;

-- Graph traversal query example:
-- SELECT *, ->reply_to->envelope.* FROM envelopes:⟨root-id⟩;
-- Deep traversal, any depth, one query — no recursive CTE needed.
```

## Implementation Plan

### Phase 1: Storage trait + SurrealDB impl (MVP)
- [ ] Define `Storage` trait in `src/storage/mod.rs`
- [ ] Define query types: `AgentFilter`, `HistoryQuery`, `AgentRecord`, `EnvelopeRecord`
- [ ] Implement `SurrealStorage` in `src/storage/surreal.rs`
- [ ] Add `surrealdb` dependency with `kv-rocksdb` feature
- [ ] Schema migration on init (DEFINE TABLE / INDEX statements)
- [ ] Wire async mirror into `ControlPlane` (fire-and-forget `store_envelope` after routing)
- [ ] Wire `register()` and `heartbeat()` to also call `Storage`
- [ ] Unit tests for each `Storage` method
- [ ] Integration test: start hub-server with storage, publish messages, query history

### Phase 2: Agent registry persistence
- [ ] Replace in-memory `AgentRegistry` with `Storage`-backed registry
- [ ] On startup, load known agents from DB
- [ ] On register/heartbeat, write to DB (async, non-blocking)
- [ ] `find_agents()` with capability + liveness filters
- [ ] Add `hub-agents` CLI tool to list/search agents

### Phase 3: Conversation threading (graph)
- [ ] Parse `reply_to` on envelopes and call `link_reply()`
- [ ] `get_thread()` uses SurrealDB graph traversal
- [ ] `list_pending()` finds messages with no reply addressed to an agent
- [ ] Add `hub-thread` CLI tool to view conversation threads

### Phase 4: Analytics trait + impl
- [ ] Define `Analytics` trait in `src/analytics/mod.rs`
- [ ] Implement `SurrealAnalytics` using SurrealQL aggregations
- [ ] Optional: `DuckdbAnalytics` for heavy OLAP (exports via Arrow/Parquet)
- [ ] Add `hub-stats` CLI tool for observability queries
- [ ] Optional: Prometheus-style metrics exporter

### Phase 5: Postgres + libSQL implementations
- [ ] `PostgresStorage` impl via `sqlx`
- [ ] `LibsqlStorage` impl via `libsql` crate
- [ ] Test parity across all three implementations

## What Does NOT Go in nats-hub's DB

- ❌ Agent memory / knowledge graphs → that's Agent Memory Labs' job
- ❌ Application state → host project owns that
- ❌ Hot-path routing decisions → that's the control plane's in-memory job
- ❌ Large binary payloads → use NATS Object Store or external blob storage
- ❌ Direct SurrealDB access to third parties → BSL safeguard

## Dependencies

```toml
# Default (SurrealDB)
[dependencies]
surrealdb = { version = "2", optional = true, features = ["kv-rocksdb"] }

# Optional (Postgres)
sqlx = { version = "0.8", optional = true, features = ["runtime-tokio", "postgres", "chrono", "json"] }

# Optional (libSQL)
libsql = { version = "0.6", optional = true, features = ["core"] }

# Optional (DuckDB analytics)
duckdb = { version = "1.105", optional = true, features = ["bundled"] }

# Required for traits
async-trait = "0.1"
```

## Research Notes (June 2026)

### Why SurrealDB won
- **Graph-native**: Conversation threading is a graph problem. SurrealDB's
  `->reply_to->envelope` traversal is a one-liner. Recursive CTEs in SQL are
  painful, limited, and get worse with depth.
- **Document-native**: `Envelope.payload` is free-form JSON. SurrealDB queries
  into nested JSON directly — no `json_extract()` hacks.
- **Multi-model in one ACID transaction**: Store envelope (document) + link
  reply (graph edge) + attach embedding (vector) — all atomically, one query.
- **Pure Rust**: No C compiler needed. Same language as the rest of the codebase.
- **Vector search built in**: Future-proof for semantic message queries.
- **v3.1.5 production-ready**: 32K stars, active development, used at scale.

### License analysis
SurrealDB core is BSL 1.1 with an extremely permissive Additional Use Grant:
- ✅ Embed in applications, ship to customers, run in production, scale freely
- ✅ Modify, create derivative works, redistribute
- ❌ Only restriction: cannot offer SurrealDB as a commercial DBaaS
- nats-hub provides messaging, not database access → restriction never triggers
- Rust SDK crate is Apache 2.0
- Change Date: 2030-01-01, converts to Apache 2.0
- Architecture safeguard: SurrealDB is always behind the `Storage` trait

### Evaluated and rejected
- **libSQL**: Good fallback but SQL recursive CTEs are painful for conversation
  threading. Document payloads need `json_extract()` hacks. Requires C compiler.
- **sled**: Still beta after 5+ years. KV-only, no SQL. Last release 2021.
- **Stoolap**: 6 months old, single contributor. Too new.
- **KiteSQL**: Smaller community, less battle-tested.
- **LanceDB**: Excellent vector DB but specialized for lakehouse/multimodal.
- **Turso (Limbo)**: Explicitly beta. Watch for future.
- **DuckDB**: Great for analytics, not for transactional storage. Kept as
  optional analytics layer.

### Key insight: CQRS fits naturally
NATS already sees every message via the control plane router. Adding an async
mirror to the DB is a one-line change in the router's routing loop. The DB's
write performance is irrelevant because writes are async fire-and-forget. What
matters is query expressiveness and read performance — which is where
SurrealDB's multi-model graph/document/vector engine shines.
