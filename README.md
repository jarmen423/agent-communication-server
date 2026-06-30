# nats-hub

A NATS-based communication layer with control plane routing for agent-to-agent and human-to-agent messaging. Built in Rust with `async-nats` and optional SurrealDB persistence.

## Why?

When building agent systems, you need a way for agents to talk to each other. nats-hub gives you:

- **Instant messaging bus** — broadcast, DM, and task channels out of the box
- **Plug-and-play workers** — any CLI, LLM API, or script can be a worker
- **Observable** — every message is persisted to SurrealDB for history, threading, and analytics
- **Portable** — embed as a crate dependency in any Rust project
- **Async-native** — built on tokio + async-nats, zero blocking calls

## Quick Start

### Prerequisites

```bash
# Install NATS server
curl -sf https://binaries.nats.dev/nats-io/nats-server/v2@latest | sh

# Start NATS
nats-server -p 4222 --jetstream
```

### Build

```bash
cargo build --release
```

### Run

```bash
# Terminal 1: Start the router (with SurrealDB persistence)
./target/release/hub-server --db-path nats_hub.db

# Terminal 2: Start a Cline worker (LLM-powered agent)
node worker.js --identity worker-1 --model "cline-pass/minimax-m3"

# Terminal 3: Delegate a task
./target/release/hub-delegate --to worker-1 --prompt "What is 2+2?" --verbose
# → 2 + 2 equals 4.

# Terminal 4: Watch message history
./target/release/hub-history --db-path nats_hub.db --tail
```

## Communication Patterns

### Broadcast (Twitter feed)

All subscribers on a channel see every message.

```rust
client.send_message("agents.broadcast", json!({"announcement": "new plan"})).await?;
```

```bash
hub-publish --channel agents.broadcast --from hermes --message "hello everyone"
```

### Direct Message (DM)

Private message to a specific agent via `meta.to` routing.

```rust
client.send_to("worker-1", "tasks", json!({"prompt": "do work"})).await?;
```

```bash
hub-publish --to worker-1 --channel tasks --from hermes --json '{"prompt":"do work"}'
```

### Reply (with correlation)

Reply to a specific message, automatically addressed to the original sender.

```rust
client.send_reply(&original_envelope, json!({"result": "work done"})).await?;
```

### Task Channel (isolated conversation)

Each task gets a unique `task.<uuid>` channel for bidirectional conversation.

```bash
hub-delegate --to worker-1 --prompt "implement feature X"
```

The flow:
1. `hub-delegate` creates `task.<uuid>`, subscribes to it
2. Sends task to worker's inbox (DM via `meta.to`)
3. Worker processes, publishes result on the task channel (broadcast)
4. `hub-delegate` receives result, prints it

Multiple parallel tasks run on separate channels — no cross-talk.

## CLI Tools

| Command | Description |
|---|---|
| `hub-server` | Run the control plane router (daemon) |
| `hub-publish` | Send a message on a channel |
| `hub-observe` | Watch messages on channels (read-only, live) |
| `hub-interact` | Interactive REPL for human messaging |
| `hub-register` | Register an agent with capabilities |
| `hub-agents` | List/search registered agents from DB |
| `hub-worker` | Universal worker: subscribe, execute, reply |
| `hub-history` | Query message history from SurrealDB |
| `hub-delegate` | Delegate a task to a worker (one command) |

## Embedding in Your Project

Add to your `Cargo.toml`:

```toml
[dependencies]
nats-hub = { path = "../nats", default-features = true }
```

For pure transport without SurrealDB (lighter dependencies):

```toml
[dependencies]
nats-hub = { path = "../nats", default-features = false, features = ["no-storage"] }
```

### Usage

```rust
use nats_hub::{HubClient, MessageKind, Envelope};
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Connect with an identity — auto-stamped on every message
    let client = HubClient::connect("nats://127.0.0.1:4222", "my-app").await?;

    // Register on the bus
    client.register(vec!["compute".into()]).await?;

    // Send a DM to a worker
    let task_id = client
        .send_to("worker-1", "tasks", json!({"prompt": "process data"}))
        .await?;

    // Subscribe to your inbox for replies
    let mut inbox = client.subscribe_inbox().await?;
    while let Some(env) = inbox.recv().await {
        println!("Reply from {}: {}", env.meta.from, env.payload);
        break;
    }

    client.drain().await;
    Ok(())
}
```

### With Persistence

```rust
use nats_hub::{SurrealStorage, Storage, HistoryQuery};
use std::sync::Arc;

let storage = SurrealStorage::connect("my_app.db").await?;
storage.migrate().await?;

// Query message history
let history = storage.query_history(
    &HistoryQuery::new().channel("tasks").limit(50)
).await?;

for record in history {
    println!("{} {} {}", record.timestamp, record.from_identity, record.kind);
}
```

## Architecture

```
Agent ──hub.send.<channel>──▶ Router ──channel.<name>──▶ Subscribers
                                  │
                   meta.to set?   │
                   ├── yes → channel.inbox.<to>   (private DM)
                   └── no  → channel.<channel>     (broadcast)

Router ──async mirror──▶ SurrealDB (message history, agent registry)
```

### Hot Path vs Cold Path

- **Hot path** (real-time): Agent → NATS → Router → Subscribers. Sub-millisecond. No DB I/O.
- **Async mirror** (write to DB): Router → `tokio::spawn` → `Storage::store_envelope()`. Fire-and-forget.
- **Cold path** (query from DB): `query_history()`, `find_agents()`, `get_thread()`. ~1ms (local SurrealDB).

### CQRS

NATS sees every message through the router. The DB is populated by an async mirror off the router. The DB's write performance barely matters — what matters is query expressiveness (indexed filtering, graph traversal, aggregations).

## Feature Flags

| Flag | Description |
|---|---|
| `default` (includes `storage-surreal`) | SurrealDB persistence with embedded RocksDB |
| `storage-surreal` | SurrealDB backend (graph-native, document-native, vector-ready) |
| `no-storage` | Pure NATS transport, no persistence layer (lighter deps) |

## Storage Trait

The `Storage` trait abstracts over database backends. SurrealDB is the default implementation.

```rust
#[async_trait]
pub trait Storage: Send + Sync {
    // Agent registry
    async fn register_agent(&self, agent: AgentRecord) -> Result<()>;
    async fn find_agents(&self, filter: &AgentFilter) -> Result<Vec<AgentRecord>>;
    async fn get_agent(&self, identity: &str) -> Result<Option<AgentRecord>>;

    // Message history
    async fn store_envelope(&self, env: &Envelope) -> Result<()>;
    async fn query_history(&self, q: &HistoryQuery) -> Result<Vec<EnvelopeRecord>>;

    // Conversation threading
    async fn link_reply(&self, reply_id: &str, parent_id: &str) -> Result<()>;
    async fn get_thread(&self, root_id: &str) -> Result<Vec<EnvelopeRecord>>;
    async fn list_pending(&self, identity: &str) -> Result<Vec<EnvelopeRecord>>;

    // Lifecycle
    async fn migrate(&self) -> Result<()>;
    async fn ping(&self) -> Result<()>;
}
```

Future backends: PostgreSQL+pgvector, libSQL. Same trait, different impl.

## Universal Workers

The `hub-worker` binary and `worker.js` are universal executors — they subscribe to a channel, receive task envelopes, execute a command, and publish results back.

```bash
# Rust worker (shells out to any CLI)
hub-worker --identity worker-1 --execute "codex" --nats-url nats://127.0.0.1:4222

# Node.js worker (Cline SDK + LLM)
node worker.js --identity worker-1 --model "cline-pass/minimax-m3"
```

Swap `--execute` or `--model` to change what the worker does. The bus doesn't care.

## Testing

```bash
cargo test
```

21 tests covering: storage (SurrealDB), agent registry, inbox routing, task channel isolation, delegate round-trip, conversation threading, list_pending.

## License

BSL 1.1 — converts to Apache 2.0 on 2030-01-01. The SurrealDB Rust SDK is Apache 2.0. See `LICENSE` for details.

## Documentation

- [`docs/PRODUCT_VISION.md`](docs/PRODUCT_VISION.md) — Full product vision and architecture
- [`docs/DATABASE_PLAN.md`](docs/DATABASE_PLAN.md) — Database and persistence design
- [`AGENTS.md`](AGENTS.md) — Guidance for AI agents working on this codebase