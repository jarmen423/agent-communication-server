# Portability & Embedding (Phase 6)

nats-hub is a library (`nats_hub`) plus a set of binaries (`hub-server`,
`hub-publish`, `hub-delegate`, …). The library is designed to be embedded in
other Rust projects as a drop-in messaging dependency.

## Add as a dependency

```toml
[dependencies]
nats-hub = { git = "https://github.com/jarmen423/nats-hub", default-features = false }

# With SurrealDB persistence:
nats-hub = { git = "https://github.com/jarmen423/nats-hub", features = ["storage-surreal"] }
```

`default-features = false` is important: the default feature pulls in
SurrealDB (embedded RocksDB), which you usually don't want when embedding.

## Feature flags

| Flag | What it enables |
|---|---|
| `default` | `storage-surreal` |
| `storage-surreal` | SurrealDB persistence (the `Storage` trait + `SurrealStorage`) and the historical `Analytics`/`hub-stats` path |
| `no-storage` | Pure NATS transport, no persistence. The live `MetricsCollector` (`hub-server --metrics-addr`) still works |

The `MetricsCollector` (Phase 4b) is **always compiled** — it has no storage
dependency, so observability is available even in `no-storage` mode.

## Minimal embed example

```rust
use nats_hub::{HubClient, MessageKind};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = HubClient::connect("nats://127.0.0.1:4222", "my-app").await?;
    client
        .send_message("agents.broadcast", json!({"hello": "world"}))
        .await?;
    Ok(())
}
```

## BSL note

SurrealDB is used under the Business Source License. The `Storage` trait keeps
all DB access behind a stable interface so the bus can be embedded without
exposing SurrealDB directly to third parties. When hosting nats-hub *as a
service*, keep `Storage` behind the trait (don't expose the DB connection) to
stay clear of the BSL DBaaS restriction.
