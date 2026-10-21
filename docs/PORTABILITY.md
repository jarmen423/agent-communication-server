# Portability & Embedding

nats-hub is a Cargo workspace with two separately licensed crates. Use
`nats-hub-client` (`nats_hub_client`, Apache-2.0) to embed messaging in your
application. It contains the protocol, NATS client, query API wire types,
records/filters, events, durable attach/catch-up helpers and pure wave
validation. It has no server, SurrealDB or RocksDB dependency.

`nats-hub` (`nats_hub`, BUSL-1.1) contains the router, storage backends, query
API implementation, orchestrator, WebSocket bridge and live metrics. It
re-exports the moved client APIs for compatibility. See
[`LICENSING.md`](../LICENSING.md) for the license split.

## Add as a dependency

```toml
[dependencies]
nats-hub-client = { git = "https://github.com/jarmen423/agent-communication-server" }

# Server embedding (BUSL-1.1), with SurrealDB persistence:
# nats-hub = { git = "https://github.com/jarmen423/agent-communication-server" }
```

No feature flags are needed for a client-only dependency. Client CLIs and
`hub-tui` query the running hub-server over NATS and never open the database.
All workspace binaries retain their names and share `target/debug/` (or
`target/release/`). Prebuilt tarballs are described in [`RELEASING.md`](RELEASING.md).

## Feature flags

| Crate / flag | What it enables |
|---|---|
| client `default` | Empty; messaging, records and query API need no features |
| client `tui` | Optional `hub-tui` dashboard (ratatui + crossterm) |
| server `default` | `storage-surreal` |
| server `storage-surreal` | SurrealDB/RocksDB persistence and historical analytics implementation |
| server `no-storage` | Compatibility server library without persistence; live metrics stay available |
| server `tui` | Forwards to the client `tui` feature for existing build commands |

`make check-no-storage` checks every client target (including TUI), rejects
server/SurrealDB/RocksDB dependencies in its transitive tree, and checks the
server library with `--no-default-features --features no-storage`. CI runs
this gate on every push. The default client build needs no C++ or libclang.

## Minimal embed example

```rust
use nats_hub_client::HubClient;
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
own license, see [`LICENSING.md`](../LICENSING.md).

SurrealDB's core is under the Business Source License. The `Storage` trait
keeps all DB access behind a stable interface, so the bus can be embedded
without exposing SurrealDB directly to third parties. When you host nats-hub
*as a service*, keep `Storage` behind the trait and don't expose the DB
connection. That keeps you clear of the BSL's DBaaS restriction.
