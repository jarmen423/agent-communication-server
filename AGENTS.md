# AGENTS.md — nats-hub

Guidance for AI agents (and humans) working on this codebase.

**Start here:** [`refocus-iteration-2.md`](refocus-iteration-2.md) has the
current iteration, the definition of done, the status board and write-scope
ownership (§6). The reply contract is in [`refocus.md`](refocus.md) §6.
[`CONTRIBUTING.md`](CONTRIBUTING.md) covers setup on any machine.

## Project Overview

**nats-hub** is a NATS-based message hub for agent-to-agent and human-to-agent
messaging. An orchestrator (a CLI, or an agent through the MCP plugin)
delegates tasks to workers (Claude Code, Codex, Cursor, Hermes, …) on any
machine, and gets results back under one reply contract. The core is a Rust
crate (`nats_hub`, `async-nats` + tokio): a control-plane router, CLIs, an
optional embedded SurrealDB mirror, a query API, a WebSocket bridge and a TUI.
Workers, bridges and the MCP server are Python.

## Architecture

```
Agent ──hub.send.<channel>──▶ Router ──channel.<name>──▶ Subscribers
                                  │
                   meta.to set?   │
                   ├── yes → channel.inbox.<to>   (private DM)
                   └── no  → channel.<channel>     (broadcast)

Router ──bounded async mirror──▶ SurrealDB (history, agent registry, sessions, waves)
CLIs / MCP / TUI ──hub.api.<op> (request-reply)──▶ hub-server ──▶ SurrealDB
Browser ──ws://…/ws──▶ hub-server WS bridge ──▶ NATS (channel.>)
```

1. Agents publish to `hub.send.<channel>` via `HubClient`, a CLI, or the Python runtime.
2. The router (`ControlPlane`) subscribes to `hub.send.>` and routes each envelope:
   - `meta.to` set → `channel.inbox.<to>` (DM)
   - `meta.to` null → `channel.<channel>` (broadcast)
3. Subscribers listen on `channel.<name>` or `channel.inbox.<identity>`.
4. Registration goes to `hub.register`, heartbeats to `hub.presence`.
5. Every envelope is mirrored to SurrealDB through a bounded queue drained by one
   writer task (`router/mirror.rs`). This is off the hot path, and when the
   queue is full the router drops the write and counts it.
6. `hub-server` is the **only** process that opens the DB (RocksDB takes a
   single-writer lock). Everything else queries it over `hub.api.<op>`.

> **Changing in iteration 2 (T1):** identity-bound subjects
> (`hub.pub.<identity>.<channel>`, `hub.api.<identity>.<op>`, …) with the router
> overwriting `meta.from`. See `refocus-iteration-2.md` §4.1. The legacy
> subjects above keep working unless `--require-bound-identity` is set.

## Code Layout

```
src/
├── lib.rs                 — module root + public re-exports
├── protocol.rs            — Envelope, Meta, MessageKind, `subjects` helpers
├── client.rs              — HubClient (connect, send/DM/reply, subscribe, sessions, register/heartbeat) + AgentRegistry cache
├── client/reply.rs        — reply-contract helpers (task result payloads, matching)
├── connect_opts.rs        — HubConnectOptions (token / user+pass / creds / nkey / require_tls) + NATS_* env
├── router.rs              — ControlPlane: subscribe hub.send.>, route, registry, presence
├── router/routing.rs      — pure routing decision + capability RoutingTable
├── router/mirror.rs       — bounded storage mirror (single writer, drop-and-count)
├── router/tests.rs        — router unit tests (no NATS)
├── query_api.rs           — hub.api.<op> request-reply server: dispatch + limits
├── query_api/handlers.rs  — agent/envelope/history/thread/session/wave ops
├── query_api/stats.rs     — stats.* ops (analytics)
├── query_api_client.rs    — ApiClient used by CLIs/TUI (never opens the DB)
├── events/                — structured progress events (MessageKind::Event): mod, display, watch targets
├── wave/                  — wave validation (disjoint scopes, deps) + spawn orchestration
├── analytics/             — Analytics trait + SurrealAnalytics (hub-stats); metrics.rs = live MetricsCollector (/metrics)
├── ws_bridge/             — WebSocket bridge + static server for the visualizer: mod, ws, http, commands, config (token/Origin checks)
├── storage/
│   ├── mod.rs             — Storage trait
│   ├── types.rs           — query/filter/record types
│   ├── surreal.rs         — SurrealStorage (embedded RocksDB, or in-memory for tests)
│   ├── schema.rs          — schema migration
│   ├── agents.rs          — agent registry methods
│   ├── envelopes.rs       — history + reply threading
│   ├── session.rs         — session CRUD
│   ├── wave.rs            — wave + wave_task CRUD
│   └── dbtime.rs          — datetime helpers
├── tui/                   — hub-tui (feature `tui`): mod (event loop), app, model, api, nats_live, event, handler, ui/
└── bin/                   — one file per CLI (see CLI Reference)
```

Python and JS (repo root):

```
worker_runtime.py          — shared worker runtime: one-shot tasks (reply contract) + stateful sessions
worker_events.py           — progress-event publishing helpers
worker_backends/           — backend types (see docs/WORKER_BACKENDS.md):
    headless_cli.py, presets.py   HeadlessCli (+ agy/hermes/grok/kilo/opencode presets)
    claude_code.py, codex_cli.py  Claude Code (`claude -p --output-format stream-json`), Codex (`codex exec --json`)
    acp_stdio.py, acp_agent.py, hermes_acp.py, grok_acp.py, opencode_acp.py   ACP over stdio
    acp_http.py, kilo_acp.py      ACP over streamable HTTP
    sdk_agent.py                  in-process SDK agents (Cursor)
    proc.py, supervision.py       subprocess / process-group plumbing
    model_catalog.py              provider-agnostic model catalog
*_worker.py                — one entrypoint per worker: claude, codex, cursor, hermes, hermes_acp, grok, grok_acp,
                             kilo, kilo_acp, opencode, opencode_acp, agy, echo (no LLM; used by tests and `make up`)
worker_supervisor.py       — spawn/stop workers on demand (visualizer "ensure worker")
remote_agent_adapter.py    — WebSocket-connected worker for remote machines
telegram_bridge.py, discord_bridge.py — human bridges (docs/BRIDGES.md)
nats_connect.py            — shared Python connect helper (token/user/creds/TLS; NATS_* env)
```

MCP server and plugins:

```
mcp_server/                — CANONICAL orchestrator MCP server (edit here)
    nats_hub_mcp.py            entrypoint; identity from NATS_HUB_IDENTITY
    hub_connection.py          connect, identity, wire helpers
    hub_tools.py, hub_query_tools.py   tool schemas
    hub_handlers.py, hub_queries.py    tool handlers
    hub_buffers.py, hub_primitives.py  subscriptions, buffers, reply-contract matcher
    hooks/session_start.py, skills/nats-hub/SKILL.md
claude-code-plugin/, codex-plugin/, hermes-plugin/   — generated copies: scripts/dev/sync_plugins.sh (--check in tests)
.claude-plugin/marketplace.json, .agents/plugins/    — marketplace manifests
```

Everything else:

```
visualizer/                — browser arcade visualizer (served by hub-server --static-dir)
scripts/dev/               — make-target helpers: setup, doctor, with_stack (throwaway stack), up, prune, sync_plugins
scripts/install_remote.sh  — install client CLIs from a GitHub release (sha256-verified) or source
scripts/dogfood_*.sh       — auth/TLS end-to-end dogfood scripts
packaging/remote/          — thin Python bundle for remote workers (FILES.txt + install.sh)
packaging/release/         — release asset packaging (used by .github/workflows/release.yml)
packaging/license/         — set_license.py (license decision pending, see LICENSE.md)
deploy/systemd/, config/   — unit files, nats-server configs (prod example, TLS example)
.github/workflows/         — ci.yml (every push), release.yml (v* tags; dry runs)
docs/                      — living docs; docs/archive/ = finished plans and handoffs
```

## Key Types

- **`Envelope`**: the wire unit. `Meta` (id, from, to, channel, timestamp, kind, reply_to) plus a free-form JSON `payload`.
- **`HubClient`**: wraps `async_nats::Client` and carries an `identity` that is stamped on every envelope. `connect()` reads auth from `NATS_*` env; `connect_with_opts()` takes `HubConnectOptions`. Methods: `send_message()`, `send_to()`, `send_reply()`, `subscribe_inbox()`, `subscribe_channel()`, `subscribe_subject()`, `subscribe_all()`, `start_session()`, `send_to_session()`, `close_session()`, `subscribe_session()`, `register()`, `heartbeat()`, `drain()`.
- **`ControlPlane`**: the router. Routes by `meta.to`, keeps the `AgentRegistry` and loads it from the DB on startup, and optionally mirrors to `Storage`.
- **`AgentRegistry`**: in-memory agent cache for hot-path queries (`find_by_capability()`, `find_alive()`, `touch()`, `deregister()`).
- **`Storage`**: persistence trait: agents, envelopes, threads (`get_thread[_bounded]`, `list_pending`), sessions, waves, `migrate()`, `ping()`. The default implementation is **`SurrealStorage`** (SurrealDB v2 embedded on RocksDB). It always sits behind the trait (the BSL safeguard for **SurrealDB's** license).
- **`ApiClient`**: request-reply client for `hub.api.<op>`: `agent.*`, `envelope.get`, `history.query`, `thread.*`, `session.*`, `wave.*`, `stats.*`.
- **`MetricsCollector`**: live counters and histograms behind `hub-server --metrics-addr`. It's always compiled, even in `no-storage`.

## Build & Run

```bash
make setup     # nats-server → .tools/bin, Python venv → .venv (idempotent)
make doctor    # toolchain check with fix hints
make build     # all bins incl. hub-tui (auto-applies the BINDGEN fix; see CONTRIBUTING.md)
make test      # Rust + Python tests against an isolated nats-server + hub-server
make lint      # fmt --check + clippy
make up        # local stack: nats-server :4222, hub-server + visualizer :9191, echo-1/echo-2
```

Each checkout or worktree uses **its own `./target`**. Dependencies are shared
through kache or sccache when installed. Never point two worktrees at one
`CARGO_TARGET_DIR`. If you run `cargo` directly, first run
`export BINDGEN_EXTRA_CLANG_ARGS="$(scripts/dev/bindgen_args.sh)"`, and keep
that value stable: changing it rebuilds RocksDB. `scripts/dev/with_stack.sh <cmd>`
wraps any command with a throwaway stack and exports `NATS_URL`.

Manual commands (against a running stack):

```bash
./target/debug/hub-server --db-path .tools/run/nats_hub.db --ws-addr 127.0.0.1:9191 --static-dir visualizer/

.venv/bin/python echo_worker.py   --identity echo-1
.venv/bin/python claude_worker.py --identity claude-1 --repo "$PWD"
.venv/bin/python codex_worker.py  --identity codex-1  --repo "$PWD"
.venv/bin/python hermes_acp_worker.py --identity hermes-acp-1

./target/debug/hub-delegate --to echo-1 --prompt "What is 2+2?" --verbose
./target/debug/hub-history --tail
./target/debug/hub-session create --worker claude-1 --from josh --prompt "Hello"
./target/debug/hub-wave create --goal "Refactor module" --from josh --tasks tasks.json
./target/debug/hub-watch --wave <wave-id>
```

## CLI Reference

| Command | Description |
|---|---|
| `hub-server [--db-path PATH] [--metrics-addr ADDR] [--ws-addr ADDR --static-dir DIR]` | Router + query API; optional Prometheus `/metrics` and WS bridge/visualizer |
| `hub-publish --channel CH [--to AGENT] [--kind KIND] --from ID --message MSG` | Send a message (broadcast or DM) |
| `hub-observe [--channel CH]` | Watch messages (read-only, live) |
| `hub-interact --from ID --channel CH` | Interactive REPL |
| `hub-register --identity ID --capabilities CAP1,CAP2` | Register an agent |
| `hub-agents [--capability CAP] [--alive SECS] [--identity ID]` | List/search agents |
| `hub-worker --identity ID --execute CMD` | Universal worker: prompt on stdin → CMD → result |
| `hub-history [--channel CH] [--from ID] [--tail]` | Query history |
| `hub-delegate --to AGENT (--prompt MSG \| --prompt-file PATH \| --prompt -) [--timeout SECS] [--verbose] [--no-wait]` | Delegate a task on an isolated task channel. Only the result goes to stdout; logs go to stderr. Ctrl-C sends a cancel (§4.2) and a second Ctrl-C exits. Exit codes: 0 done, 1 worker error, 2 timeout, 3 channel closed, 4 cancelled, 130 interrupted. |
| `hub-session create/send/close/list/status` | Stateful multi-turn sessions |
| `hub-watch [--session\|--wave\|--agent\|--channel\|--all]` | Watch structured progress events |
| `hub-wave create/spawn/status/close/list` | Parallel wave orchestration |
| `hub-thread show/pending` | Reply chains and unanswered messages |
| `hub-stats [--since DUR] [--agent ID] [--top-channels N] [--json]` | Rates, latency, activity, hotspots, error rate |
| `hub-tui [--nats-url URL] [--refresh-secs N] [--alive-secs N] [--feed-cap N]` | Terminal dashboard (feature `tui`) |

Release tarballs contain all of these (see `docs/RELEASING.md`).

## Workers

`worker_runtime.py` plus a backend from `worker_backends/`. See
`docs/WORKER_BACKENDS.md` for the types (HeadlessCli, Claude Code, Codex,
AcpAgent over stdio, AcpHttp, SdkAgent) and which entrypoint uses which. The Rust
`hub-worker --execute` is the universal "any CLI" worker. Every worker
implements the reply contract (`refocus.md` §6): progress as `status`/`event`
on `channel.task.<id>`, then exactly one terminal
`{"status": "done"|"error", "task_id", "result", "error"}`.

**Human bridges:** `telegram_bridge.py` (reference) and `discord_bridge.py`
subscribe to `channel.inbox.<identity>`, forward NATS→human, and publish
human→NATS. They run standalone. See `docs/BRIDGES.md`.

## Distributed hub + auth

One central `nats-server` + `hub-server`. Remote machines join as NATS clients
over `wss://` (or `tls://`/`nats://` on a trusted network).

- Rust: `HubConnectOptions::from_env` (`NATS_TOKEN`, `NATS_USER`/`NATS_PASSWORD`, `NATS_CREDENTIALS_FILE`, `NATS_NKEY`, `NATS_REQUIRE_TLS`).
- Python: `nats_connect.py` (`--token`, `--user/--password`, `--ca-file`, `--credentials-file`, … with the same env fallback).
- Docs: `docs/SECURITY.md`, `docs/OPERATOR_HUB.md`, `docs/JOIN_HUB.md`, `docs/REMOTE_INSTALL.md`, `docs/REMOTE_AGENTS.md`.
- Deploy: `deploy/systemd/*`, `config/nats-server.prod.conf.example`. Dogfood: `scripts/dogfood_token_auth.sh`, `scripts/dogfood_wss_tls.sh`.
- Client install: `scripts/install_remote.sh` (release binaries, sha256-verified), `packaging/remote/install.sh` (Python worker bundle).

## Communication Patterns

- **Broadcast**: `send_message(channel, payload)`. No `meta.to`, so every subscriber sees it.
- **DM**: `send_to(agent, channel, payload)` sets `meta.to`, and the router delivers to `channel.inbox.<agent>`.
- **Reply**: `send_reply(&original, payload)`, a DM with `meta.reply_to` = the original's **message id**.
- **Task channel**: `hub-delegate`/MCP create `task.<uuid>`, subscribe first, then DM the worker `{prompt, task_channel}`.
- **Session**: `hub-session` creates `session.<uuid>` for multi-turn on `channel.session.<uuid>`.
- **Wave**: `hub-wave` creates `wave.<id>` + `wave.<id>.task.<task_id>`: parallel tasks with dependencies and a merge gate.
- **Events**: workers publish `MessageKind::Event` `{event_type, data}`. Watch them with `hub-watch`.

## Conventions

- **Identity**: set once at connect and stamped on every envelope. Never put it in payloads. The MCP server takes it from `NATS_HUB_IDENTITY`, never from tool arguments.
- **Wire format**: JSON `Envelope`s. Payloads are free-form JSON.
- **Subjects**: `hub.send.<channel>` (publish), `channel.<name>` (broadcast), `channel.inbox.<identity>` (DM), `channel.task.<uuid>`, `channel.session.<uuid>`, `channel.wave.<id>[.task.<task_id>]`, `hub.api.<op>` (query API), `hub.register`, `hub.presence`.
- **Message kinds**: `message`, `control`, `human`, `status`, `event`.
- **Persistence**: only `hub-server` opens the DB; everything else uses the query API.
- **Async everywhere**: tokio + async-nats, `tokio::sync::Mutex`, `tokio::process::Command`. Python uses asyncio. No blocking calls.
- **File size**: keep files under ~400 LOC. Split into focused modules.
- **Tests that need NATS** read `NATS_URL` and never hard-code `:4222`.
- **Plugins**: edit `mcp_server/`, then run `scripts/dev/sync_plugins.sh`. Never edit the plugin copies.

## Testing

```bash
make test        # both suites
make test-rust   # = scripts/dev/with_stack.sh cargo test --features tui
make test-py     # = scripts/dev/with_stack.sh .venv/bin/python -m pytest -q tests/python
```

`with_stack.sh` starts a private `nats-server` on a random port and a `hub-server`
with a temp DB, then exports `NATS_URL` and `NATS_HUB_TEST_STACK=1`. Without
it, NATS-dependent Rust tests skip and Python `live` tests are skipped.

At the time of writing, `main` has about **156 Rust tests** (51 unit tests in
`src/`, 105 in 16 integration files) and about **100 Python tests** in 11
files. CI (`.github/workflows/ci.yml`) runs fmt, build, both suites and the
`no-storage` library check on every push.

| Rust integration file | Covers |
|---|---|
| `storage_surreal.rs`, `agent_registry.rs`, `threads.rs`, `sessions.rs`, `waves.rs` | Storage: envelopes, registry (DB + cache), threading, session/wave CRUD, wave validation |
| `inbox_routing.rs`, `task_channels.rs` | DM routing, reply correlation, task-channel isolation (live NATS) |
| `router_routing.rs`, `router_query_api.rs`, `router_stack.rs` | Routing decisions, query-API dispatch and limits, live hub-server checks |
| `e2e_delegation.rs` | Real `hub-delegate` ↔ `hub-worker`/Python worker round trips (reply contract) |
| `ws_bridge.rs` | Static-path traversal, Origin allowlist, WS token auth |
| `analytics.rs`, `metrics_tests.rs`, `events.rs`, `hub_connect_opts.rs` | Analytics, live metrics, event formatting, connect options |

Python (`tests/python/`): `test_runtime_contract.py` (reply contract),
`test_worker_{claude,codex,headless,acp,supervisor}.py` (fake-CLI backend
tests), `test_mcp_server.py` / `test_mcp_followups.py` (MCP server),
`test_smoke.py` (live echo round trip), and `test_install_release.py` /
`test_license_switch.py` (release installers, license script). Fake CLIs live
in `tests/python/fixtures/`.

## Feature Flags

| Flag | Description |
|---|---|
| `default` (= `storage-surreal`) | SurrealDB persistence |
| `storage-surreal` | SurrealDB on embedded RocksDB; also the `Analytics` trait + `hub-stats` |
| `no-storage` | Pure NATS transport, no persistence. `MetricsCollector` still available. Checked in CI |
| `tui` | ratatui + crossterm for `hub-tui` (off by default) |

## Common Tasks

- **Add a CLI**: add a file in `src/bin/`, register it under `[[bin]]` in `Cargo.toml` (with `required-features` if it needs storage), and use `HubClient`/`ApiClient`. The release workflow picks up every `hub-*` binary automatically.
- **Add a message kind**: extend `MessageKind` in `protocol.rs` (serde rename).
- **Add a query-API op**: add a handler in `src/query_api/handlers.rs` and dispatch it in `query_api.rs`; expose it through `ApiClient` callers.
- **Add a storage backend**: implement `Storage` in a new `src/storage/` module behind a feature flag.
- **Add a worker type**: add a backend in `worker_backends/` and a thin `<name>_worker.py`, plus fake-CLI tests in `tests/python/`. See `docs/WORKER_BACKENDS.md`.
- **Add an MCP tool**: add it to `mcp_server/` (schema + handler), then run `scripts/dev/sync_plugins.sh`.
- **Add a file to the remote bundle**: add it to `packaging/remote/FILES.txt`. `install.sh` and the release bundle both read that list.
- **Cut a release**: see `docs/RELEASING.md`.

## Documentation

- `README.md`: pitch, quick start, docs map
- `CONTRIBUTING.md`: dev setup, tests, conventions, troubleshooting
- `refocus-iteration-2.md` (current) and `refocus.md` (iteration 1, reply contract §6)
- `docs/WORKER_BACKENDS.md`: worker backend types and entrypoints
- `docs/SECURITY.md`, `docs/OPERATOR_HUB.md`, `docs/JOIN_HUB.md`, `docs/QUICK_START_REMOTE.md`: distributed hub + auth
- `docs/REMOTE_INSTALL.md`, `docs/REMOTE_AGENTS.md`: remote workers
- `docs/BRIDGES.md`: human bridges
- `docs/VISUALIZER.md`, `docs/pets.md`: browser visualizer
- `docs/PORTABILITY.md`: embedding, feature flags, Storage trait
- `docs/RELEASING.md`: release workflow, dry runs, install from release
- `docs/PRODUCT_VISION.md`, `docs/DATABASE_PLAN.md`: vision and persistence design
- `docs/archive/`: finished phase plans and handoffs (historical; see its README)
- `LICENSE.md`: license decision status (pending)
