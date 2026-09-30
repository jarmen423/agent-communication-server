# nats-hub — Refocus Brief (Sep 2026)

> Living doc. The **status board** (§5) is ground truth for the refocus sprint:
> update it whenever a task changes state. Other planning docs (`docs/PHASE*`,
> `.planning/`) are historical.

## 1. TL;DR

Between Jun 29 and Aug 2 2026 (52 commits) we built very widely: transport,
persistence, sessions, waves, events, analytics, metrics, a TUI, an arcade
visualizer, about 14 worker entrypoints, 2 bridges, 3 agent plugins, and
distributed-hub auth. Every phase in the docs is marked ✅.

The core loop is where the gaps are. An orchestrator sends a task to a worker
and should reliably get the answer back, but that loop is **broken on several
paths**. Nothing is secure yet (identity is self-asserted), and the agents we
actually use, Claude Code and Codex, are not supported as workers.

**This sprint goes deep, not wide.** Freeze new features until the core loop is
true end to end and tested.

## 2. The vision, as it evolved

| Source | What it says |
|---|---|
| `IDEA.md` | A lightweight *secure* broker: discovery registry, API-key identity, observability dashboard |
| `docs/PRODUCT_VISION.md` | A universal agent communication layer and embeddable Rust crate. Agents, humans and bridges share one bus, and everything is persisted for audit and evaluation |
| `TODO.md` | Coordination for agent teams across multiple machines, a broadcast when an agent is editing a file, provider health checks, a robust usage SKILL, a themeable visualizer that agents can customize |

**Common thread:** your coding agents (Claude Code, Codex, …) on several machines
coordinate through one hub while a human watches and steers.

### North star (to be confirmed)

- **A (recommended):** a coordination hub for *our own* multi-machine coding
  agents, used every day ourselves.
- **B:** an embeddable crate for other Rust developers.
- **C:** an installable consumer product (visualizer, pets, theming).

B and C both work better once A is solid, and the sprint below serves all three.

## 3. Where we are vs. the vision

| Area | Docs claim | Reality (evidence) | ~% |
|---|---|---|---|
| Routing: broadcast, DM, task channels | ✅ | Works. Core NATS only (no JetStream), so a DM to an offline agent or a send while hub-server is down is silently lost | 70 |
| Persistence | ✅ | Store/query works, but **the schema is never applied**: `DEFINE FIELD … AT` should be `ON` (`src/storage/surreal.rs:559`), and the errors are logged only at debug level | 50 |
| Delegation loop | ✅ | **Broken.** `hub-worker` replies to the sender's inbox while `hub-delegate` listens only on the task channel, so it times out. MCP `delegate_task` treats the first `started` event as the answer | 35 |
| Sessions / waves | ✅ | Wave state lives in the CLI process (a wave stays "running" forever if that process exits). The hub never runs `verify_cmd`. No cycle detection. Worker sessions are memory-only | 40 |
| Workers | ✅ | Many backends, but **no Claude Code or Codex worker**: `worker_supervisor.py:53` maps both to `echo_worker.py`. The JS workers can't run from a fresh clone | 35 |
| Orchestrator surface (MCP plugin) | ✅ | 20 tools, copied byte-for-byte into 3 plugins. Delegation is broken, there's no way to read an inbox or wait for a result, auth is ignored, and `from` is a free parameter | 30 |
| Distributed + auth | ✅ | Connecting remotely with token/TLS works. **Identity doesn't:** anyone can set `meta.from`, the default ACL lets any agent read every inbox, and `hub.api.*` writes and the WS bridge have no authentication | 30 |
| Observability (TUI, visualizer, stats, `/metrics`) | ✅ | Strongest area. The visualizer is one 3,400-line HTML file and ships about 5.8 MB of third-party fan-art sprites | 75 |
| Portability / install | ✅ | `no-storage` doesn't compile. No LICENSE (plugins say MIT, crate says BSL). No CI. `jfrie` paths in 21 files | 15 |

**Build/test on the dev box (2026-09-29):** the default build failed because
RocksDB bindgen couldn't find `stdbool.h`. With the fix, about 95 Rust tests
pass (0 fail, 2 ignored). NATS-dependent tests silently skipped because no
`nats-server` was installed. `ControlPlane`, the query API, the WS bridge and a
real delegate↔worker round-trip have **zero** tests. There are no Python tests.

### Security issues (these matter before any network exposure)

1. **Path traversal in the static file server:** a lexical `starts_with` check after `dir.join(path)` (`src/ws_bridge.rs:427`).
2. **The WS bridge has no Origin check or token.** Any web page you visit can open `ws://127.0.0.1:9191/ws`, read all traffic, and spawn always-approve workers via `ensure_worker`.
3. **Identity isn't bound to credentials.** `docs/SECURITY.md` claims the router re-scopes senders; it doesn't.

## 4. Sprint plan — "make the core loop true"

**Goal:** one orchestrator (Claude Code) delegates to a real Claude Code or Codex
worker, possibly on another machine, gets the result back reliably, and a human
watches it happen.

**Step 0 — dev environment (blocks everything):** builds on any machine,
`nats-server` available, one-command setup/doctor/test, no machine-specific
paths, CI.

**Week 1 — correctness**

1. One reply contract (§6) across the Rust worker, delegate, Python runtime and MCP, plus an end-to-end test harness.
2. Storage correctness: schema `ON`, loud migration failures, heartbeat `touch()`, default limits, a sane `list_pending`.
3. Real Claude Code and Codex workers, with timeouts, process-group kill, and non-zero exit treated as an error.
4. One shared MCP server for all plugins: `delegate_async`, `task_status`, `wait_for_message`, auth, identity from env.

**Week 2 — trustworthy and installable**

5. WS bridge hardening (path traversal, Origin check, token), then identity binding (`hub.send.<identity>.<channel>` plus NATS permissions).
6. JetStream-backed inboxes, so offline agents and router restarts stop losing messages.
7. Onboarding: `make up` demo, workflow SKILL, archive of historical docs.

**Deferred:** more bridges, visualizer theming and the customization skill, the
DuckDB OLAP backend, TUI phases 5–6. The file-lock broadcast (`TODO.md`) is the
first *new* feature after this sprint.

## 5. Status board

Legend: ⬜ not started · 🟡 in progress · ✅ done · ⛔ blocked

| ID | Task | Runs on | Branch | Status | Evidence / notes |
|---|---|---|---|---|---|
| S0 | Dev environment fixes + collaborator setup (`CONTRIBUTING.md`, `make doctor/setup/test`, CI) | local (orchestrator) | `refocus/dev-env` | ✅ | **PR #1** — CI green on GitHub (clean Ubuntu, 14m). 2026-09-29: fresh clone → `make setup && make build && make test` green (28 Rust result groups ok, 0 failed; pytest 5 passed incl. live echo round-trip). NATS tests now actually run under `with_stack.sh`. `make up` + `hub-delegate --to echo-1` → `echo: olleh`; visualizer HTTP 200. Zero compiler warnings; fmt clean. Fixed a timing-flaky liveness test. |
| L1 | Reply contract + end-to-end delegation harness | local subagent | `refocus/l1-reply-contract` | ✅ | **PR #8**, awaiting merge. Verified by the orchestrator on main+L1 in an isolated build: 115 Rust passed / 0 failed. Two Python live tests were load-sensitive (fixed sleep / spawn-time clock); both now wait on the worker's first registration (`5a2d394`), then 34/34 passed twice at load ~18. Known: register+presence write conflicts in SurrealDB. They're harmless, and L2's single-writer mirror removes them. Brief: `.planning/refocus/L1-reply-contract.md` |
| L2 | Storage + router correctness (schema, heartbeat, limits, `list_pending`, `no-storage` build) | local subagent | `refocus/l2-storage` | 🟡 | Brief: `.planning/refocus/L2-storage.md` |
| L3 | Claude Code + Codex workers, backend hardening, Python tests | local subagent | `refocus/l3-workers` | ✅ | **PR #2**, awaiting merge. R2 has merged, so the WS-bridge exposure that blocked it is closed. Being updated to current main and live-verified (supervisor behind the authenticated bridge) in Devin session 077d9c8d. Verified independently: `make test-py` 57 passed; real smoke tests claude → `pong` (24.5s), codex → `pong` (63.4s). Brief: `.planning/refocus/L3-workers.md` |
| R1 | Unified MCP orchestrator server (one copy, fixed delegate, async tools, auth) | remote agent | `refocus/r1-mcp` | ✅ | **Merged (PR #3)** 2026-09-30. The review found 4 majors, all fixed before merge (`d54f965`): reconnect survival, bounded subscriptions and trackers, Hermes per-call timeout, `mcp>=1.19`. Also wave fail-fast. Verified on main+R1+R2 in an isolated target dir: 104 Rust / 24 Python passed, plugin copies in sync. Minor follow-ups are in §7. Brief: `.planning/refocus/R1-mcp.md` |
| R2 | WS bridge + visualizer transport hardening (traversal, Origin, token) | remote agent | `refocus/r2-ws-bridge` | ✅ | **Merged (PR #4)** 2026-09-30. Probed against a live server: traversal (plain and encoded) returns 403 with no leak; bad Origin → 403; missing or wrong token → 401; valid token → 101; a 0.0.0.0 bind without a token refuses to start. Minor follow-ups are in §7. Brief: `.planning/refocus/R2-ws-bridge.md` |

**Process:** each task works on its own branch and opens a PR against `main`.
The orchestrator verifies every task by re-running `make lint && make test`
(self-reports alone are not trusted), then updates this board. Briefs live in
`.planning/refocus/`.

**Where verification runs (keep this machine light):**
- **Build and test gates → GitHub CI.** Every PR gets fmt, build, and Rust +
  Python tests on a clean runner. To verify a PR against a moved `main`,
  merge `main` into the branch (or re-run CI) instead of building locally.
- **Full-environment checks → Devin sessions** (the `devin-handoff` skill):
  a live stack, supervisor/bridge flows, long builds, and browser checks.
  Each session has its own VM, so there's no local disk or CPU cost.
- **Local → review plus quick, targeted runs** in the worktree's own `./target`,
  with the shared kache cache. Run `make prune` after merges.

> **Lesson (2026-09-29):** never point several worktrees at one
> `CARGO_TARGET_DIR`. The crate's artifacts collide, and cargo silently tests
> another worktree's code. Each worktree uses its own `./target`; dependencies
> are shared through the kache/sccache compile cache (set automatically by
> `scripts/dev/lib.sh`). Run `make prune` after merges. See `CONTRIBUTING.md` §2.

### Write-scope ownership (to prevent merge collisions)

| ID | Owns (may edit) | Must NOT edit |
|---|---|---|
| L1 | `src/bin/hub_worker.rs`, `src/bin/hub_delegate.rs`, reply helpers in `src/client.rs`, `worker_runtime.py`, `worker_events.py`, `tests/e2e_*.rs` (new), `tests/inbox_routing.rs`, `tests/python/test_runtime_contract.py`, `tests/python/test_smoke.py` | `src/router.rs`, `src/storage/**`, `worker_backends/**`, plugins |
| L2 | `src/storage/**`, `src/router.rs`, `src/query_api.rs`, `src/query_api_client.rs`, `src/analytics/**`, cfg gates in `src/lib.rs`, `tests/{storage_surreal,agent_registry,threads,analytics,router_*}.rs` | `src/client.rs`, `src/bin/hub_worker.rs`, `src/bin/hub_delegate.rs`, Python |
| L3 | `worker_backends/**`, `*_worker.py` (except `echo_worker.py`), new `claude_worker.py`/`codex_worker.py`, `worker_supervisor.py`, `config/provider_models.json`, `docs/WORKER_BACKENDS.md`, `tests/python/test_worker_*.py`, `tests/python/fixtures/**` | `worker_runtime.py`, `worker_events.py`, Rust |
| R1 | `claude-code-plugin/**`, `codex-plugin/**`, `hermes-plugin/**`, `.agents/**`, new `.claude-plugin/**`, new `mcp_server/**`, new `scripts/dev/sync_plugins.sh`, `tests/python/test_mcp_*.py` | Rust, worker files |
| R2 | `src/ws_bridge.rs` (may split into `src/ws_bridge/`), `src/bin/hub_server.rs`, the WS-connection code in `visualizer/`, `docs/SECURITY.md`, `docs/VISUALIZER.md`, `tests/ws_bridge*.rs` | `src/router.rs`, storage, Python |

**Shared files** (`README.md`, `AGENTS.md`, `refocus.md`, `Cargo.toml`,
`Makefile`, `requirements*.txt`) are integrated by the orchestrator. Tasks
propose changes to them in their final report instead of editing them. The
exception is a genuinely required new dependency; flag it clearly.

## 6. Shared invariant — the reply contract

Every task that sends or receives task results implements this exactly:

1. **Delegation.** The delegator DMs the worker (`meta.to = <worker>`,
   `kind = message`) with payload `{"prompt": …, "task_channel": "task.<id>"}`.
   The delegator subscribes to `channel.task.<id>` **before** sending.
2. **`meta.reply_to` always holds a message ID** (the ID of the envelope being
   replied to). It never holds a channel name. The channel travels in
   `payload.task_channel`.
3. **The worker publishes progress** on the task channel as broadcast
   (`meta.to = null`): `kind = status` (`{"status":"working"|…}`) and
   `kind = event` (`{event_type, data}`), with
   `meta.reply_to = <task envelope id>`.
4. **The worker publishes exactly one terminal result** on the task channel:
   `kind = message`, `meta.reply_to = <task envelope id>`, payload
   `{"status":"done"|"error", "task_id": <task envelope id>, "result": <string|null>, "error": <string|null>}`.
5. **The delegator's result** is the first envelope on the task channel with
   `kind = message` and either `meta.reply_to == <task envelope id>` **or**
   `payload.task_id == <task envelope id>`. Every `status`/`event` envelope is
   progress, never the result. The `payload.task_id` fallback exists because
   the Python runtime already sends it today, while its `meta.reply_to` still
   holds the channel name until L1 lands. Consumers must accept both.
6. **Plain DM without `payload.task_channel`:** the worker replies by DM to
   `meta.from` (`meta.to = <sender>`, `meta.reply_to = <id>`), using the same
   result payload shape.

## 7. Backlog after the sprint

**Review follow-ups from R1 (MCP, PR #3), all minor:**
- Plugin hooks bypass `connect_nats`, so they report "unreachable" on an auth hub.
- `.mcp.json` hardcodes the identity and URL (should use `${NATS_HUB_IDENTITY:-…}` and `${NATS_URL:-…}`).
- `hooks/` and `SKILL.md` aren't covered by `sync_plugins.sh --check`. `hermes-plugin/hooks/` is dead code.
- `wait_for_message` returns stale messages by default (it should default to `last_seq`).
- `start_session` ignores a failed `session.create`.
- `tracker.error` isn't surfaced for waves.
- The Codex and Hermes hook output formats are unverified.

**Review follow-ups from R2 (WS bridge, PR #4), all minor:**
- The default Origin list misses `127.0.0.1` for `localhost`/`0.0.0.0` binds and misses port 80. Allow-list entries aren't normalized.
- A token containing `+` breaks in the browser. URL-encode the banner, or restrict the token charset.
- The token comparison isn't constant-time. An Origin that isn't valid UTF-8 skips the check.
- `--ws-identity` doesn't cover stop/resume. The shared NATS connection doesn't retry its first connect.
- Startup guards should be testable. `make up` and the Hermes plugin should append `?token=` when `HUB_WS_TOKEN` is set.
- The docs should prefer `HUB_WS_TOKEN` over `--ws-token`, which is visible in `ps`.

**Other:**

- JetStream inbox durability (sprint item 6)
- Identity binding via subject plus NATS permissions (second half of sprint item 5)
- `no-storage` feature compiles; `query_api` goes behind `dyn Storage`
- Server-side wave orchestration (state in hub-server, cycle detection, event-sender checks, liveness TTL)
- Split the visualizer into modules; replace third-party sprites with a theme manifest
- Resolve the license (BSL vs MIT) and add a LICENSE file
- File-lock broadcast (`TODO.md`)
