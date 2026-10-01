# Portability & Embedding

nats-hub is a library (`nats_hub`) plus a set of binaries (`hub-server`,
`hub-publish`, `hub-delegate`, …). You can embed the library in another Rust
project as a messaging dependency. The binaries are also published as
prebuilt release tarballs (see [`RELEASING.md`](RELEASING.md)).

## Add as a dependency

```toml
[dependencies]
# Transport only: HubClient, Envelope, events, MetricsCollector. No SurrealDB/RocksDB.
nats-hub = { git = "https://github.com/jarmen423/agent-communication-server", default-features = false, features = ["no-storage"] }

# With SurrealDB persistence (Storage trait, SurrealStorage, Analytics):
nats-hub = { git = "https://github.com/jarmen423/agent-communication-server" }
```

`default-features = false` matters. The default feature pulls in SurrealDB
(embedded RocksDB, a C++ build of about 10 minutes), which you usually don't
want when embedding. CI checks the transport-only build on every push
(`cargo check --lib --no-default-features --features no-storage`).

## Feature flags

| Flag | What it enables |
|---|---|
| `default` | `storage-surreal` |
| `storage-surreal` | SurrealDB persistence (the `Storage` trait + `SurrealStorage`) and the historical `Analytics`/`hub-stats` path |
| `no-storage` | Pure NATS transport, no persistence. The live `MetricsCollector` (`hub-server --metrics-addr`) still works |
| `tui` | `hub-tui` terminal dashboard (ratatui + crossterm). Off by default, so library users never pull in TUI deps |

The `MetricsCollector` is **always compiled**. It has no storage dependency, so
observability is available even in `no-storage` mode.

## Minimal embed example

```rust
use nats_hub::HubClient;
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Identity is stamped on every envelope. Auth/TLS come from NATS_* env
    // (NATS_TOKEN, NATS_USER/NATS_PASSWORD, NATS_CREDENTIALS_FILE, NATS_NKEY,
    // NATS_REQUIRE_TLS); use HubClient::connect_with_opts to pass them explicitly.
    let client = HubClient::connect("nats://127.0.0.1:4222", "my-app").await?;
    client.register(vec!["compute".into()]).await?;

    // Broadcast, DM, and wait for a reply on your inbox.
    client.send_message("agents.broadcast", json!({"hello": "world"})).await?;
    client.send_to("worker-1", "tasks", json!({"prompt": "process data"})).await?;
    let mut inbox = client.subscribe_inbox().await?;
    if let Some(env) = inbox.recv().await {
        println!("reply from {}: {}", env.meta.from, env.payload);
    }
    client.drain().await?;
    Ok(())
}
```

## Persistence: the `Storage` trait

With `storage-surreal`, `SurrealStorage` implements the `Storage` trait
(`src/storage/mod.rs`). It covers the agent registry, message history,
reply threading, sessions and waves. Future backends (Postgres, libSQL)
implement the same trait.

```rust
use nats_hub::{HistoryQuery, Storage, SurrealStorage};

let storage = SurrealStorage::connect("my_app.db").await?;
storage.migrate().await?;
for r in storage.query_history(&HistoryQuery::new().channel("tasks").limit(50)).await? {
    println!("{} {} {}", r.timestamp, r.from_identity, r.kind);
}
```

Embedded RocksDB allows **one process per DB path**. While a `hub-server` owns
the DB, other processes query it over NATS through the query API (`ApiClient`,
`hub.api.*` subjects), as the `hub-*` CLIs do. They never open the DB directly.

## BSL note (SurrealDB)

This section is about **SurrealDB's** license, not nats-hub's. For nats-hub's
own license, see [`LICENSE.md`](../LICENSE.md).

SurrealDB's core is under the Business Source License. The `Storage` trait
keeps all DB access behind a stable interface, so the bus can be embedded
without exposing SurrealDB directly to third parties. When you host nats-hub
*as a service*, keep `Storage` behind the trait and don't expose the DB
connection. That keeps you clear of the BSL's DBaaS restriction.
