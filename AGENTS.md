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
├── connect_opts.rs           — HubConnectOptions (token/creds/TLS) + env fallback
├── events/                   — structured progress events (MessageKind::Event)
├── wave/                     — wave validation + spawn orchestration
├── router.rs                 — ControlPlane (routing daemon, async DB mirror, WS bridge push)
├── query_api.rs              — NATS request-reply API (hub.api.>) — DB ops routed through server
├── query_api_client.rs       — ApiClient for CLI tools (avoids RocksDB lock contention)
├── ws_bridge.rs              — WebSocket bridge + static file server (for visualizer)
├── storage/
│   ├── mod.rs                — Storage trait + query types
│   ├── surreal.rs            — SurrealStorage impl (embedded RocksDB)
│   ├── session.rs            — session CRUD (split from surreal.rs)
│   └── wave.rs               — wave + wave_tasks CRUD
└── bin/
    ├── hub_server.rs         — runs the control plane router (daemon, --db-path + --metrics-addr flags)
    ├── hub_publish.rs        — send a message on a channel (broadcast or --to DM; --kind sets MessageKind)
    ├── hub_observe.rs        — watch messages on channels (read-only)
    ├── hub_interact.rs       — interactive REPL for human messaging
    ├── hub_register.rs       — register an agent with capabilities
    ├── hub_agents.rs         — list/search registered agents from DB
    ├── hub_worker.rs         — universal worker: subscribe, execute command, reply
    ├── hub_history.rs        — query message history from SurrealDB
    ├── hub_delegate.rs       — one-command task delegation with task channels
    ├── hub_session.rs        — stateful multi-turn sessions
    ├── hub_watch.rs          — watch structured progress events (real-time)
    ├── hub_wave.rs           — parallel wave orchestration with merge gates
    └── hub_thread.rs         — view conversation threads and pending messages
    └── hub_stats.rs          — observability/analytics CLI (Phase 4a: rates, latency, activity, hotspots, error rate)
    └── hub_tui.rs            — ratatui terminal dashboard (feature = "tui")
```

TUI library modules (`src/tui/`, feature-gated behind `tui`):

```
src/tui/
├── mod.rs        — run() entry + unified tokio::select! event loop
├── app.rs        — App state (focus, selections, feed ring buffer, live map)
├── model.rs      — AgentRow/FeedLine/PanelFocus/Snapshot + status-merge logic
├── api.rs        — refresh_snapshot() batched query-API calls (tokio::join!)
├── nats_live.rs  — spawn_live_listener() over HubClient::subscribe_all()
├── event.rs      — AppEvent enum
├── handler.rs    — apply_event() pure state transitions
└── ui/           — ratatui renderers (layout, agents, sessions, waves, feed, chrome)
```

Python workers: `worker_runtime.py`, `worker_events.py`, `worker_backends/`, `hub_worker.js`.
Remote agents: `remote_agent_adapter.py` (WebSocket-connected workers for distributed teams).
Auth helper (Python): `nats_connect.py` — shared token/TLS/creds connect used by all Python clients.

## Distributed hub + auth (shipped)

One central `nats-server` + `hub-server`; remote machines join as NATS **clients** over `ws://` or `wss://`.

- Python clients: `nats_connect.py` (`--token`, `--user/--password`, `--ca-file`, `--credentials-file`, …; `NATS_*` env fallback)
- Rust clients + hub-server: `HubConnectOptions::from_env` (same `NATS_*` env)
- Docs: `docs/SECURITY.md`, `OPERATOR_HUB.md`, `JOIN_HUB.md`, `REMOTE_INSTALL.md`, `REMOTE_AGENTS.md`
- Deploy: `deploy/systemd/*`, `config/nats-server.prod.conf.example`
- Dogfood: `scripts/dogfood_token_auth.sh`, `scripts/dogfood_wss_tls.sh`
- Execution archive: `.planning/execution/ROADMAP.md` (status COMPLETE)

## Key Types

- **`Envelope`** — the wire unit. Contains `Meta` (id, from, to, channel, timestamp, kind, reply_to) + free-form JSON `payload`.
- **`HubClient`** — wraps `async_nats::Client`. Carries an `identity: String` auto-stamped on every envelope. Methods: `send_message()`, `send_to()` (DM), `send_reply()`, `subscribe_inbox()`, `subscribe_channel()`, `subscribe_subject()`, `start_session()`, `send_to_session()`, `close_session()`, `subscribe_session()`, `register()`, `heartbeat()`.
- **`ControlPlane`** — the router. Subscribes to `hub.send.>`, routes by `meta.to`. Optionally mirrors to `Storage` (async, fire-and-forget). Loads agents from DB on startup.
- **`AgentRegistry`** — in-memory cache of known agents. Hot-path queries use this; cold-path queries go to `Storage`. Methods: `register()`, `find_by_capability()`, `find_alive()`, `touch()`, `deregister()`.
- **`Storage`** — trait abstracting the persistence backend. Default impl: `SurrealStorage` (embedded RocksDB). Provides `store_envelope()`, `query_history()`, `find_agents()`, `get_thread()`, `list_pending()`, session CRUD, wave CRUD, `migrate()`, `ping()`.
- **`SurrealStorage`** — SurrealDB v2 embedded via RocksDB. Graph-native (conversation threading), document-native (free-form JSON), zero-config. Always behind the `Storage` trait (BSL safeguard).

## Build & Run

**Start here:** `refocus.md` (current sprint, status board, write-scope ownership,
reply contract) and `CONTRIBUTING.md` (setup on any machine).

```bash
make setup     # nats-server → .tools/bin, Python venv → .venv (idempotent)
make doctor    # toolchain check with fix hints
make build     # all bins incl. hub-tui (auto-applies BINDGEN fix; see CONTRIBUTING.md)
make test      # Rust + Python tests against an isolated nats-server + hub-server
make up        # local stack: nats-server :4222, hub-server + visualizer :9191, echo-1/echo-2
```

If you invoke `cargo` directly, first run `export BINDGEN_EXTRA_CLANG_ARGS="$(scripts/dev/bindgen_args.sh)"`.
The value must stay stable, because changing it rebuilds RocksDB. To wrap any
command with a throwaway stack, use `scripts/dev/with_stack.sh <cmd>`, which
exports `NATS_URL`.

Manual commands (against a running stack; `NATS_URL` defaults to `nats://127.0.0.1:4222`):

```bash
# Router with persistence + visualizer (make up does this)
./target/debug/hub-server --db-path .tools/run/nats_hub.db --ws-addr 127.0.0.1:9191 --static-dir visualizer/

# Workers (Python, from the repo venv)
.venv/bin/python echo_worker.py --identity echo-1
.venv/bin/python hermes_acp_worker.py --identity hermes-acp-1
.venv/bin/python cursor_worker.py --identity cursor-worker-1 --repo "$PWD"
node hub_worker.js --type cline --identity cline-worker-1 --model "cline-pass/minimax-m3"   # needs make setup-js

# Delegate, observe
./target/debug/hub-delegate --to echo-1 --prompt "What is 2+2?" --verbose
./target/debug/hub-history --tail

# Stateful session (multi-turn)
./target/debug/hub-session create --worker cursor-worker-1 --from josh --prompt "Hello"
./target/debug/hub-session send <session-id> --from josh --message "Follow up"
./target/debug/hub-watch --session <session-id>

# Parallel wave orchestration
./target/debug/hub-wave create --goal "Refactor module" --from josh --tasks tasks.json
./target/debug/hub-wave spawn <wave-id> --from josh
./target/debug/hub-watch --wave <wave-id>
```

## CLI Reference

| Command | Description |
|---|---|
| `hub-server [--db-path PATH] [--metrics-addr ADDR]` | Run the control plane router (optional Prometheus `/metrics` endpoint) |
| `hub-publish --channel CH [--to AGENT] [--kind KIND] --from ID --message MSG` | Send a message (broadcast or DM) |
| `hub-observe [--channel CH]` | Watch messages (read-only, live) |
| `hub-interact --from ID --channel CH` | Interactive REPL |
| `hub-register --identity ID --capabilities CAP1,CAP2` | Register an agent |
| `hub-agents [--capability CAP] [--alive SECS] [--identity ID]` | List/search agents |
| `hub-worker --identity ID --execute CMD` | Universal worker |
| `hub-history [--channel CH] [--from ID] [--tail]` | Query history |
| `hub-delegate --to AGENT --prompt MSG [--timeout SECS] [--verbose]` | Delegate a task |
| `hub-session create/send/close/list/status` | Stateful multi-turn sessions |
| `hub-watch [--session\|--wave\|--agent\|--channel\|--all]` | Watch structured progress events |
| `hub-wave create/spawn/status/close/list` | Parallel wave orchestration |
| `hub-thread show/pending` | View reply chains and unanswered messages |
| `hub-stats [--since DUR] [--agent ID] [--top-channels N] [--json]` | Analytics: rates, latency, activity, hotspots, error rate (Phase 4a) |
| `hub-server --ws-addr ADDR --static-dir DIR` | WebSocket bridge + visualizer static files |
| `hub-worker.js --type <cline\|agy\|hermes\|cursor> --identity <name>` | Universal worker (single CLI, all backend types) |
| `hub-tui [--nats-url URL] [--refresh-secs N] [--alive-secs N] [--feed-cap N]` | ratatui terminal dashboard (feature `tui`) — agents/sessions/waves + live feed |

## Python workers (typed backends)

`worker_runtime` + **`worker_backends/`** types. See `docs/WORKER_BACKENDS.md`.

| Type | Examples |
|------|----------|
| **HeadlessCli** | `agy_worker.py` (`HeadlessCliSpec` in `presets.py`) |
| **SdkAgent** | `cursor_worker.py` |
| **AcpAgent** | `hermes_acp_worker.py` (`HermesAcpBackend`); Cursor blocked until upstream ACP transport is exposed here |
| **$ExecCli** | Rust `hub-worker --execute` |

**Human bridges** (Phase 5): `telegram_bridge.py` is the reference adapter
(`docs/BRIDGES.md`). A bridge subscribes to `channel.inbox.<identity>`, forwards
NATS→human, and publishes human→NATS. Runs standalone (not via `hub-worker`).

## Communication Patterns

- **Broadcast**: `send_message(channel, payload)` — no `meta.to`, all subscribers see it
- **DM**: `send_to(agent, channel, payload)` — sets `meta.to`, router routes to `channel.inbox.<agent>`
- **Reply**: `send_reply(&original, payload)` — DM + `meta.reply_to` correlation ID
- **Task channel**: `hub-delegate` creates `task.<uuid>`, isolated bidirectional conversation
- **Session**: `hub-session` creates `session.<uuid>`, multi-turn on `channel.session.<uuid>`
- **Wave**: `hub-wave` creates `wave.<id>` + `wave.<id>.task.<task_id>`, parallel tasks with deps
- **Events**: workers publish `MessageKind::Event` with `{event_type, data}`; observe via `hub-watch`

## Conventions

- **Identity**: set once at `HubClient::connect(url, identity)`. Auto-stamped on every envelope. Never manually append identity to payloads.
- **Wire format**: all messages are JSON `Envelope` structs. Payloads are free-form JSON.
- **Subject conventions**: `hub.send.<channel>` for publishing, `channel.<name>` for broadcast, `channel.inbox.<identity>` for DM, `channel.task.<uuid>` for task channels, `channel.session.<uuid>` for sessions, `channel.wave.<id>` / `channel.wave.<id>.task.<task_id>` for waves.
- **Message kinds**: `message`, `control`, `human`, `status`, `event` (see `MessageKind` enum).
- **Persistence**: SurrealDB (embedded RocksDB) via the `Storage` trait. All envelopes + agent registrations are async-mirrored to the DB. Agent registry persists across restarts.
- **Async everywhere**: all I/O is async (tokio + async-nats). No blocking calls. Use `tokio::sync::Mutex`, not `std::sync::Mutex`. Use `tokio::process::Command`, not `std::process::Command`.
- **File size**: keep files under ~400 LOC. Split into focused modules if growing.
- **Feature flags**: `default = ["storage-surreal"]`. Use `no-storage` for pure transport without SurrealDB.

## Testing

```bash
make test        # both suites
make test-rust   # = scripts/dev/with_stack.sh cargo test --features tui
make test-py     # = scripts/dev/with_stack.sh .venv/bin/python -m pytest -q tests/python
```

`with_stack.sh` starts a private `nats-server` on a random port and a `hub-server`
with a temp DB, then exports `NATS_URL` and `NATS_HUB_TEST_STACK=1`. Tests that need
NATS **must read `NATS_URL`** and never hard-code `:4222`. Without the stack,
NATS-dependent tests skip, and Python `live` tests are skipped.

Current count: about 95 Rust tests (29 lib unit + 11 integration files) plus Python
smoke tests in `tests/python/`. Known gaps (see `refocus.md`): no tests for
`ControlPlane` routing, `query_api`, `ws_bridge`, or a Rust delegate↔worker round-trip.

## Feature Flags

| Flag | Description |
|---|---|
| `default` (includes `storage-surreal`) | SurrealDB persistence |
| `storage-surreal` | SurrealDB with embedded RocksDB; also enables the `Analytics` trait + `hub-stats` (Phase 4a) |
| `no-storage` | Pure NATS transport, no persistence; `hub-server --metrics-addr` (Phase 4b `MetricsCollector`) still available |
| `tui` | ratatui + crossterm for the `hub-tui` terminal dashboard (off by default; library consumers don't pull TUI deps) |

**Analytics layering**: the live `MetricsCollector` (Phase 4b) is always compiled and has no storage dependency — it works in `--features no-storage`. The historical `Analytics`/`SurrealAnalytics` trait (Phase 4a, `hub-stats`) lives behind `storage-surreal` since it reads persisted `envelopes`.

## Common Tasks

- **Add a new CLI tool**: add a binary in `src/bin/`, register in `Cargo.toml` under `[[bin]]`, use `HubClient` or `Envelope` from `nats_hub`.
- **Add a new message kind**: extend `MessageKind` in `protocol.rs`, update serde rename.
- **Add a storage backend**: implement the `Storage` trait in a new module under `src/storage/`, add a feature flag in `Cargo.toml`.
- **Add a human bridge**: create a specialized `hub-worker` with a transport-specific `--execute` command (e.g. Telegram, SMS, Postiz).

## Documentation

- `README.md` — quickstart, embedding guide, CLI reference
- `docs/PHASE3_PLAN.md` — Phase 3a/3b/3c plan and completion status
- `docs/PRODUCT_VISION.md` — full product vision, communication patterns, worker design
- `docs/DATABASE_PLAN.md` — database architecture and implementation roadmap
- `docs/WORKER_BACKENDS.md` — Python worker backend types and event/wave integration
- `docs/REMOTE_AGENTS.md` — WebSocket remote agent adapter for distributed teams
- `docs/VISUALIZER.md` — visualizer startup, browser commands, and troubleshooting