# nats-hub — Refocus Brief (Sep 2026)

> Living doc. The **status board** (§5) is ground truth for the refocus sprint:
> update it whenever a task changes state. Other planning docs (`docs/PHASE*`,
> `.planning/`) are historical.

## 1. TL;DR

> **Sprint outcome (2026-09-30): complete.** Every task (S0, L1–L3, R1–R2, the
> R1/R2 review follow-ups, and the build-cache work) is merged to `main`. CI on
> the combined `main` is green: **155 Rust tests and 93 Python tests, 0
> failures**. The goal works end to end: an orchestrator (Claude Code via the
> MCP plugin) delegates to a real Claude Code or Codex worker through an
> authenticated bridge, gets the result back under one reply contract, and a
> human can watch it. The two big remaining gaps are **durable delivery**
> (JetStream) and **identity bound to credentials**. See §3 (before/after) and
> §7 (next sprint).

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

| Area | Before the sprint (2026-09-29) | After the sprint (2026-09-30) | ~% before → after |
|---|---|---|---|
| Routing: broadcast, DM, task channels | Works. Core NATS only (no JetStream), so a DM to an offline agent or a send while hub-server is down is silently lost | Unchanged semantics. Routing is now a pure, tested `route_subject()`, and there's no per-message `flush()`. **Offline DMs are still lost** (JetStream is next sprint) | 70 → 75 |
| Persistence | Store/query works, but **the schema is never applied** (`DEFINE FIELD … AT`), and errors are logged only at debug level | Schema applied (v2) with loud migration errors. Native datetimes, with a one-time legacy conversion verified on a real pre-L2 DB. Indexes added; `list_pending`/`get_thread` are bounded and indexed; a single-writer mirror with a drop metric (#9) | 50 → 85 |
| Delegation loop | **Broken.** `hub-worker` replies to the sender's inbox while `hub-delegate` listens only on the task channel. MCP `delegate_task` takes the `started` event as the answer | One reply contract (§6) across Rust worker, `hub-delegate`, Python runtime and MCP. Covered by end-to-end tests: timeout, 1 MB prompt, DM replies (#8, #3) | 35 → 90 |
| Sessions / waves | Wave state lives in the CLI process. The hub never runs `verify_cmd`. No cycle detection. Worker sessions are memory-only | Claude/Codex workers resume by session id, and the MCP wave spawn fails fast. **Server-side wave orchestration is still to do** | 40 → 50 |
| Workers | No Claude Code or Codex worker (the supervisor mapped both to echo). JS workers can't run from a fresh clone | Real Claude Code and Codex workers; real-CLI smoke tests returned `pong`. Hardened backends: timeouts, process-group kill, non-zero exit counts as an error. Shared ACP client. The supervisor logs, restarts and cleans up children (#2). `package.json` added | 35 → 80 |
| Orchestrator surface (MCP plugin) | Copied 3×. Delegation broken, no inbox or wait tools, auth ignored, `from` is a free parameter | One canonical source with a drift check. `delegate_async`/`task_status`/`wait_for_task`/`read_inbox`/`wait_for_message`. Auth via `connect_nats`; identity from env or the plugin manifest. Bounded buffers; survives reconnects. Verified on a token-auth hub (#3, #5) | 30 → 85 |
| Distributed + auth | Remote token/TLS connect works. **Identity doesn't:** anyone can set `meta.from`. `hub.api.*` writes and the WS bridge are unauthenticated | The WS bridge is authenticated: token, Origin allowlist, traversal fix, and it refuses to start on a non-loopback bind without a token (#4, #6). MCP and hooks work on an auth hub. **Still open:** `meta.from` is self-asserted, and `hub.api.*` writes are unauthenticated | 30 → 50 |
| Observability (TUI, visualizer, stats, `/metrics`) | Strongest area. The visualizer is one 3,400-line HTML file with third-party sprites | Added a storage-mirror drop metric. Visualizer unchanged (split/theming is backlog) | 75 → 75 |
| Portability / install | `no-storage` doesn't compile. No LICENSE. No CI. `jfrie` paths in 21 files | `make setup/doctor/build/test/up/prune`, CI on every branch, requirements files, no machine paths. `no-storage` compiles. Shared kache build cache and a slim dev profile. **Still open:** LICENSE, release binaries | 15 → 60 |

**Baseline build/test on the dev box (2026-09-29):** the default build failed because
RocksDB bindgen couldn't find `stdbool.h`. With the fix, about 95 Rust tests
pass (0 fail, 2 ignored). NATS-dependent tests silently skipped because no
`nats-server` was installed. `ControlPlane`, the query API, the WS bridge and a
real delegate↔worker round-trip have **zero** tests. There are no Python tests.

### Security issues (these matter before any network exposure)

Status after the sprint: 1 and 2 are **fixed** (#4, #6, live-probed); 3 is **open** (next sprint).

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

1. ✅ One reply contract (§6) across the Rust worker, delegate, Python runtime and MCP, plus an end-to-end test harness.
2. ✅ Storage correctness: schema `ON`, loud migration failures, heartbeat `touch()`, default limits, a sane `list_pending`.
3. ✅ Real Claude Code and Codex workers, with timeouts, process-group kill, and non-zero exit treated as an error.
4. ✅ One shared MCP server for all plugins: `delegate_async`, `task_status`, `wait_for_message`, auth, identity from env.

**Week 2 — trustworthy and installable**

5. ◐ WS bridge hardening (path traversal, Origin check, token) is ✅ done. Identity binding (`hub.send.<identity>.<channel>` plus NATS permissions) is ⬜ **next sprint**.
6. ⬜ **Next sprint:** JetStream-backed inboxes, so offline agents and router restarts stop losing messages.
7. ◐ Onboarding: `make up` demo ✅, workflow SKILL ✅ (rewritten in #3), archive of historical docs ⬜.

**Deferred:** more bridges, visualizer theming and the customization skill, the
DuckDB OLAP backend, TUI phases 5–6. The file-lock broadcast (`TODO.md`) is the
first *new* feature after this sprint.

## 5. Status board

Legend: ⬜ not started · 🟡 in progress · ✅ done · ⛔ blocked

| ID | Task | Runs on | Branch | Status | Evidence / notes |
|---|---|---|---|---|---|
| S0 | Dev environment fixes + collaborator setup (`CONTRIBUTING.md`, `make doctor/setup/test`, CI) | local (orchestrator) | `refocus/dev-env` | ✅ | **PR #1** — CI green on GitHub (clean Ubuntu, 14m). 2026-09-29: fresh clone → `make setup && make build && make test` green (28 Rust result groups ok, 0 failed; pytest 5 passed incl. live echo round-trip). NATS tests now actually run under `with_stack.sh`. `make up` + `hub-delegate --to echo-1` → `echo: olleh`; visualizer HTTP 200. Zero compiler warnings; fmt clean. Fixed a timing-flaky liveness test. |
| L1 | Reply contract + end-to-end delegation harness | local subagent | `refocus/l1-reply-contract` | ✅ | **Merged to main (#8)** 2026-09-30. Verified by the orchestrator on main+L1 in an isolated build: 115 Rust passed / 0 failed. Two Python live tests were load-sensitive (fixed sleep / spawn-time clock); both now wait on the worker's first registration (`5a2d394`), then 34/34 passed twice at load ~18. Known: register+presence write conflicts in SurrealDB. They're harmless, and L2's single-writer mirror removes them. Brief: `.planning/refocus/L1-reply-contract.md` |
| L2 | Storage + router correctness (schema, heartbeat, limits, `list_pending`, `no-storage` build) | local subagent | `refocus/l2-storage` | ✅ | **Merged to main (#9)** 2026-09-30. Live-verified by Devin (session 4d2ddd2b). **Pre-L2 DB upgrade:** migration v0→2, 0 unconvertible rows, idempotent on restart; old data reads back and new writes work. **Combined L1+L2:** 147 Rust / 34 Python passed, and `no-storage` compiles. **~97s soak with 4 re-registering workers:** 0 write conflicts. **Pending:** cleared only by a `kind=message` reply. `natshub_storage_mirror_dropped_total` is exported. Brief: `.planning/refocus/L2-storage.md` |
| L3 | Claude Code + Codex workers, backend hardening, Python tests | local subagent | `refocus/l3-workers` | ✅ | **Merged to main (#2)** 2026-09-30. Updated to current main (`b0ceb7b`) and live-verified in a Devin session: make lint/test green (76 Python passed). Behind the authenticated bridge, `ensure_worker` with a valid token and Origin spawned the Claude worker, and a delegate round-trip returned the result; no token or a wrong token → 401, evil Origin → 403; stop mid-turn left no orphaned processes. Earlier: real claude/codex smoke tests → `pong`. Brief: `.planning/refocus/L3-workers.md` |
| R1 | Unified MCP orchestrator server (one copy, fixed delegate, async tools, auth) | remote agent | `refocus/r1-mcp` | ✅ | **Merged (PR #3)** 2026-09-30. The review found 4 majors, all fixed before merge (`d54f965`): reconnect survival, bounded subscriptions and trackers, Hermes per-call timeout, `mcp>=1.19`. Also wave fail-fast. Verified on main+R1+R2 in an isolated target dir: 104 Rust / 24 Python passed, plugin copies in sync. Follow-ups done in F1 (#5). Brief: `.planning/refocus/R1-mcp.md` |
| R2 | WS bridge + visualizer transport hardening (traversal, Origin, token) | remote agent | `refocus/r2-ws-bridge` | ✅ | **Merged (PR #4)** 2026-09-30. Probed against a live server: traversal (plain and encoded) returns 403 with no leak; bad Origin → 403; missing or wrong token → 401; valid token → 101; a 0.0.0.0 bind without a token refuses to start. Follow-ups done in F2 (#6). Brief: `.planning/refocus/R2-ws-bridge.md` |
| F1 | R1 review follow-ups: hooks via `connect_nats`, identity defaults, drift check covers hooks/skills, fresh-only `wait_for_message`, `start_session` errors | remote agent (Devin) | `refocus/r1-followups` | ✅ | **Merged (#5).** Independently live-verified by a second Devin session on a real token-auth hub (5/5 checks). The orchestrator found and fixed a marketplace-install bug: identity was derived from the dir name, which is a version string in `~/.claude/plugins/cache/…` (`bf6deba`). |
| F2 | R2 review follow-ups: Origin normalization, NATS connect retry, `--ws-identity` coverage, URL-encoded/constant-time token, testable startup guards | remote agent (Devin) | `refocus/r2-followups` | ✅ | **Merged (#6).** Orchestrator review requested 2 fixes (scheme-case default port, Hermes `?`/`&`); both landed (`f07c973`). CI green. |
| B1 | Build cache + disk: kache/sccache via `lib.sh`, slim dev debuginfo, `make prune`/`make clean`, CI on every branch | local (orchestrator) | `refocus/status` | ✅ | **Merged (#7).** Triggered by the disk filling up (~20–40 GB per checkout; hub-server alone was 787 MB). |

**Final (2026-09-30):** all tasks merged; CI on combined `main` is green (**155 Rust / 93 Python, 0 failed**).

**Process (solo repo):** each task works on its own branch. The orchestrator
verifies it independently (self-reports alone are not trusted), then merges it
into `main` **locally** and pushes. GitHub PRs are opened only on request.
Briefs live in `.planning/refocus/`.

**Where verification runs (keep this machine light):**
- **Build and test gates → GitHub CI.** Every pushed branch gets fmt, build, and Rust +
  Python tests on a clean runner for every branch push. To verify a branch
  against a moved `main`, merge `main` into it and push, instead of building locally.
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

## 7. Backlog / next sprint candidates

The R1/R2 review follow-ups are done (F1/F2 in §5). What's left, roughly in priority order:

**Next sprint ("make it trustworthy"):**
1. **JetStream-backed inboxes.** DMs to offline agents and sends during a router restart are still lost (sprint item 6).
2. **Identity bound to credentials.** Publish on `hub.send.<identity>.<channel>`, enforce it with NATS permissions, and have the router overwrite `meta.from`. Lock `hub.api.*` writes to a privileged user. Fix the default ACL that lets any agent read every inbox (sprint item 5b).
3. **Server-side wave orchestration.** State lives in hub-server; add cycle detection, event-sender checks, liveness TTL → task failure, and surface `tracker.error` for waves in the MCP server.

**Small fixes:**
- `hub-delegate` writes INFO logs to **stdout** when `RUST_LOG` is set, which breaks piping its result and 2 e2e tests. Send logs to stderr (found by the L2 live check).
- Add `cargo check --lib --no-default-features --features no-storage` to CI (proposed by L2).
- `hub-delegate --prompt-file` / stdin: the command-line argument limit is 128 KiB (L1).
- Per-turn progress handlers can cross-talk between concurrent session turns (L1/L3).
- `worker_supervisor.py` (403 LOC) and `src/client.rs` (452 LOC) are over the size guideline.
- `test_session_start_hook_output_shape` may flake under heavy load (timeout raised to 30s; watch it).

**Later:**
- Analytics loads whole time ranges into memory; push the aggregation down into the DB (L2).
- Split the visualizer into modules; replace third-party sprites with a theme manifest (this enables the customization skill in `TODO.md`).
- Resolve the license (BSL vs MIT) and add a LICENSE file; add release binaries.
- Archive historical planning docs (`docs/PHASE*`, `.planning/execution`).
- **File-lock broadcast** (`TODO.md`): the first *new* feature once the trust items land.
