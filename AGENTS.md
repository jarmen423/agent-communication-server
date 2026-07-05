# AGENTS.md — nats-hub

Guidance for AI agents working on this codebase.

## Project Overview

**nats-hub** is a NATS-based communication layer with a control plane router for agent-to-agent and human-to-agent messaging. Built in Rust with `async-nats` and optional SurrealDB persistence.

## Architecture

```
Agent ──hub.send.<channel>──▶ Router ──channel.<name>──▶ Subscribers
                                  │
                   meta.to set?   │
                   ├── yes → channel.inbox.<to>   (private DM)
                   └── no  → channel.<channel>     (broadcast)

Router ──async mirror──▶ SurrealDB (message history, agent registry)
```

1. Agents publish to `hub.send.<channel>` via `HubClient` or CLI tools
2. The control plane router subscribes to `hub.send.>` and routes each envelope:
   - If `meta.to` is set → routes to `channel.inbox.<to>` (private DM)
   - If `meta.to` is null → routes to `channel.<channel>` (broadcast)
3. Subscribers listen on `channel.<name>` (broadcast) or `channel.inbox.<identity>` (DM)
4. Registration goes to `hub.register`, heartbeats to `hub.presence`
5. Every envelope is async-mirrored to SurrealDB (fire-and-forget, off hot path)

## Code Layout

```
src/
├── lib.rs                    — module root, public API exports, doc comments
├── protocol.rs               — Envelope, Meta, MessageKind, subject conventions
├── client.rs                 — HubClient + AgentRegistry (in-memory cache)
├── router.rs                 — ControlPlane (routing daemon, async DB mirror)
├── storage/
│   ├── mod.rs                — Storage trait + query types (AgentFilter, HistoryQuery, etc.)
│   └── surreal.rs            — SurrealStorage impl (embedded RocksDB)
└── bin/
    ├── hub_server.rs         — runs the control plane router (daemon, --db-path flag)
    ├── hub_publish.rs        — send a message on a channel
    ├── hub_observe.rs        — watch messages on channels (read-only)
    ├── hub_interact.rs       — interactive REPL for human messaging
    ├── hub_register.rs       — register an agent with capabilities
    ├── hub_agents.rs         — list/search registered agents from DB
    ├── hub_worker.rs         — universal worker: subscribe, execute command, reply
    ├── hub_history.rs        — query message history from SurrealDB
    └── hub_delegate.rs       — one-command task delegation with task channels
```

## Key Types

- **`Envelope`** — the wire unit. Contains `Meta` (id, from, to, channel, timestamp, kind, reply_to) + free-form JSON `payload`.
- **`HubClient`** — wraps `async_nats::Client`. Carries an `identity: String` auto-stamped on every envelope. Methods: `send_message()`, `send_to()` (DM), `send_reply()`, `subscribe_inbox()`, `subscribe_channel()`, `register()`, `heartbeat()`.
- **`ControlPlane`** — the router. Subscribes to `hub.send.>`, routes by `meta.to`. Optionally mirrors to `Storage` (async, fire-and-forget). Loads agents from DB on startup.
- **`AgentRegistry`** — in-memory cache of known agents. Hot-path queries use this; cold-path queries go to `Storage`. Methods: `register()`, `find_by_capability()`, `find_alive()`, `touch()`, `deregister()`.
- **`Storage`** — trait abstracting the persistence backend. Default impl: `SurrealStorage` (embedded RocksDB). Provides `store_envelope()`, `query_history()`, `find_agents()`, `get_thread()`, `list_pending()`, `migrate()`, `ping()`.
- **`SurrealStorage`** — SurrealDB v2 embedded via RocksDB. Graph-native (conversation threading), document-native (free-form JSON), zero-config. Always behind the `Storage` trait (BSL safeguard).

## Build & Run

```bash
# Build all binaries
CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats cargo build

# Start NATS server (prerequisite)
nats-server -p 4222 --jetstream

# Start the router (with SurrealDB persistence)
./target/debug/hub-server --db-path nats_hub.db

# Start a Cline worker (Node.js + Cline SDK)
node hub_worker.js --type cline --identity cline-worker-1 --model "cline-pass/minimax-m3"

# Start a Hermes ACP worker (JSON-RPC 2.0)
python3 hermes_acp_worker.py --identity hermes-acp-1

# Start a Cursor worker (Cursor SDK)
python3 cursor_worker.py --identity cursor-worker-1 --repo /home/jfrie/nats

# Delegate a task
./target/debug/hub-delegate --to worker-1 --prompt "What is 2+2?" --verbose

# Watch history
./target/debug/hub-history --db-path nats_hub.db --tail
```

**Note**: Set `CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats` to avoid filling `/` (193G disk). The `/data` partition has 369G.

## CLI Reference

| Command | Description |
|---|---|
| `hub-server [--db-path PATH]` | Run the control plane router |
| `hub-publish --channel CH [--to AGENT] --from ID --message MSG` | Send a message |
| `hub-observe [--channel CH]` | Watch messages (read-only, live) |
| `hub-interact --from ID --channel CH` | Interactive REPL |
| `hub-register --identity ID --capabilities CAP1,CAP2` | Register an agent |
| `hub-agents [--capability CAP] [--alive SECS] [--identity ID]` | List/search agents |
| `hub-worker --identity ID --execute CMD` | Universal worker |
| `hub-history [--channel CH] [--from ID] [--tail]` | Query history |
| `hub-delegate --to AGENT --prompt MSG [--timeout SECS] [--verbose]` | Delegate a task |
| `hub-session create/send/close/list/status` | Stateful multi-turn sessions |
| `hub-worker.js --type <cline|agy|hermes|cursor> --identity <name>` | Universal worker (single CLI, all backend types) |

## Python workers (typed backends)

`worker_runtime` + **`worker_backends/`** types. See `docs/WORKER_BACKENDS.md`.

| Type | Examples |
|------|----------|
| **HeadlessCli** | `agy_worker.py` (`HeadlessCliSpec` in `presets.py`) |
| **SdkAgent** | `cursor_worker.py` |
| **AcpAgent** | `hermes_acp_worker.py` (`HermesAcpBackend`); Cursor blocked until upstream ACP transport is exposed here |
| **$ExecCli** | Rust `hub-worker --execute` |

## Communication Patterns

- **Broadcast**: `send_message(channel, payload)` — no `meta.to`, all subscribers see it
- **DM**: `send_to(agent, channel, payload)` — sets `meta.to`, router routes to `channel.inbox.<agent>`
- **Reply**: `send_reply(&original, payload)` — DM + `meta.reply_to` correlation ID
- **Task channel**: `hub-delegate` creates `task.<uuid>`, isolated bidirectional conversation

## Conventions

- **Identity**: set once at `HubClient::connect(url, identity)`. Auto-stamped on every envelope. Never manually append identity to payloads.
- **Wire format**: all messages are JSON `Envelope` structs. Payloads are free-form JSON.
- **Subject conventions**: `hub.send.<channel>` for publishing, `channel.<name>` for broadcast, `channel.inbox.<identity>` for DM, `channel.task.<uuid>` for task channels.
- **Message kinds**: `message`, `control`, `human`, `status` (see `MessageKind` enum).
- **Persistence**: SurrealDB (embedded RocksDB) via the `Storage` trait. All envelopes + agent registrations are async-mirrored to the DB. Agent registry persists across restarts.
- **Async everywhere**: all I/O is async (tokio + async-nats). No blocking calls. Use `tokio::sync::Mutex`, not `std::sync::Mutex`. Use `tokio::process::Command`, not `std::process::Command`.
- **File size**: keep files under ~400 LOC. Split into focused modules if growing.
- **Feature flags**: `default = ["storage-surreal"]`. Use `no-storage` for pure transport without SurrealDB.

## Testing

```bash
CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats cargo test
```

21 tests across 4 test files:
- `tests/storage_surreal.rs` (5): envelope store/query, agent registry, threading, ping
- `tests/agent_registry.rs` (10): capability/alive/touch/deregister filters (DB + in-memory)
- `tests/inbox_routing.rs` (3): DM routing, reply correlation, subject format
- `tests/task_channels.rs` (3): task channel isolation, delegate round-trip, list_pending

**Note**: Tests that require NATS server will skip gracefully if it's not running.

## Feature Flags

| Flag | Description |
|---|---|
| `default` (includes `storage-surreal`) | SurrealDB persistence |
| `storage-surreal` | SurrealDB with embedded RocksDB |
| `no-storage` | Pure NATS transport, no persistence |

## Common Tasks

- **Add a new CLI tool**: add a binary in `src/bin/`, register in `Cargo.toml` under `[[bin]]`, use `HubClient` or `Envelope` from `nats_hub`.
- **Add a new message kind**: extend `MessageKind` in `protocol.rs`, update serde rename.
- **Add a storage backend**: implement the `Storage` trait in a new module under `src/storage/`, add a feature flag in `Cargo.toml`.
- **Add a human bridge**: create a specialized `hub-worker` with a transport-specific `--execute` command (e.g. Telegram, SMS, Postiz).

## Documentation

- `README.md` — quickstart, embedding guide, CLI reference
- `docs/PRODUCT_VISION.md` — full product vision, communication patterns, worker design
- `docs/DATABASE_PLAN.md` — database architecture and implementation roadmap