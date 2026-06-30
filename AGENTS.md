# AGENTS.md — nats-hub

Guidance for AI agents working on this codebase.

## Project Overview

**nats-hub** is a NATS-based communication layer with a control plane router for agent-to-agent and human-to-agent messaging. Built in Rust with `async-nats`.

## Architecture

```
Agent Client → hub.send.<channel> → Control Plane Router → channel.<name> → Subscribers
```

1. Agents publish to `hub.send.<channel>` via `HubClient` or CLI tools
2. The control plane router (`hub-server`) subscribes to `hub.send.>` and routes each message to `channel.<channel>`
3. Subscribers listen on `channel.<name>` (specific) or `channel.>` (all)
4. Registration goes to `hub.register`, heartbeats to `hub.presence`

## Code Layout

```
src/
├── lib.rs           — module root, re-exports HubClient, Envelope, MessageKind, ControlPlane, Storage types
├── protocol.rs      — Envelope, Meta, MessageKind, subject conventions
├── client.rs        — HubClient (connect, send, subscribe, register, heartbeat) + AgentRegistry (in-memory cache)
├── router.rs        — ControlPlane (routing daemon, optional Storage async mirror)
├── storage/
│   ├── mod.rs       — Storage trait + query types (AgentFilter, HistoryQuery, AgentRecord, EnvelopeRecord)
│   └── surreal.rs   — SurrealStorage impl (embedded RocksDB, graph-native threading)
└── bin/
    ├── hub_server.rs    — runs the control plane router (daemon, with --db-path flag)
    ├── hub_publish.rs   — send a message on a channel
    ├── hub_observe.rs   — watch messages on channels (read-only)
    ├── hub_interact.rs  — interactive REPL for human messaging
    ├── hub_register.rs  — register an agent with capabilities
    └── hub_agents.rs    — list/search registered agents from DB (--capability, --alive, --identity)
```

See `docs/PRODUCT_VISION.md` for the full architecture vision and communication patterns.

## Key Types

- **`Envelope`** — the wire unit. Contains `Meta` (id, from, channel, to, timestamp, kind, reply_to) + free-form JSON `payload`.
- **`HubClient`** — wraps `async_nats::Client`. Carries an `identity: String` that is auto-stamped onto every envelope. Connect once, send many.
- **`ControlPlane`** — the router. Subscribes to `hub.send.>`, re-publishes to `channel.<name>`. Optionally mirrors all envelopes + agent registrations to `Storage` (async, fire-and-forget).
- **`AgentRegistry`** — in-memory cache of known agents (populated from registrations + presence, and loaded from `Storage` on startup). Hot-path queries use this; cold-path queries go to `Storage`.
- **`Storage`** — trait abstracting the persistence backend. Default impl: `SurrealStorage` (embedded RocksDB). Provides `store_envelope()`, `query_history()`, `find_agents()`, `get_thread()`, `list_pending()`, `migrate()`, `ping()`.
- **`SurrealStorage`** — SurrealDB v2 embedded via RocksDB. Graph-native (conversation threading), document-native (free-form JSON payloads), zero-config. Always behind the `Storage` trait (BSL safeguard).

## Build & Run

```bash
cargo build --release                          # build all binaries
nats-server -c config/nats-server.conf         # start NATS server (prerequisite)
./target/release/hub-server                    # start the control plane router

# In separate terminals:
./target/release/hub-register --identity agent-alpha --capabilities compute
./target/release/hub-publish --channel agents.broadcast --from agent-alpha --message "hello"
./target/release/hub-observe                    # watch all channels
./target/release/hub-interact --from josh --channel agents.broadcast
```

## Conventions

- **Identity**: provided once at `HubClient::connect(url, identity)`. Auto-stamped on every envelope via `Envelope::new(self.identity.clone(), ...)`. Never manually append identity to payloads.
- **Wire format**: all messages are JSON `Envelope` structs. Payloads are free-form JSON — agents decide their own schemas per channel.
- **Subject conventions**: defined in `protocol::subjects`. `hub.send.<channel>` for publishing, `channel.<name>` for routed delivery, `hub.register` / `hub.presence` for control.
- **Message kinds**: `message`, `control`, `human`, `status` (see `MessageKind` enum).
- **Persistence**: SurrealDB (embedded RocksDB) via the `Storage` trait. All envelopes + agent registrations are async-mirrored to the DB. Agent registry persists across restarts. Message history is queryable.
- **File size**: keep files under ~400 LOC. Split into focused modules if growing.

## Testing

```bash
cargo test
```

Tests live in `tests/`.

## Common Tasks

- **Add a new CLI tool**: add a binary in `src/bin/`, register it in `Cargo.toml` under `[[bin]]`, use `HubClient` or `Envelope` from `nats_hub`.
- **Add a new message kind**: extend `MessageKind` in `protocol.rs`, update serde rename if needed.
- **Add persistence**: `AgentRegistry` is the natural place — wrap in file or NATS KV-backed store.
