# nats-hub — Refocus, Iteration 2: take every gap to 100%

> Living doc. **§5 Status board** is ground truth. Iteration 1 (`refocus.md`)
> is complete; its reply contract (`refocus.md` §6) still applies unchanged.
> Started 2026-09-30.

## 1. Goal

Iteration 1 fixed the core loop: delegate → result, real Claude Code and Codex
workers, a unified MCP server, an authenticated bridge, and correct storage. It
left each area somewhere between 50% and 90%. **Iteration 2 closes every area to
100% of a written definition of done** (§2). "100%" means every acceptance
criterion below is met and verified. Security in particular is never "finished",
so for auth, 100% means the threat model in `docs/SECURITY.md` is fully
enforced and tested.

The work runs in **two waves**. Wave 1 is five tasks: three native subagents and
two Devin sessions. Wave 2 takes whatever is left, and some of it depends on
wave 1 landing.

## 2. Definition of 100%, per area

| Area | End of iteration 1 | 100% means (acceptance criteria) | Task | Wave |
|---|---|---|---|---|
| **Routing & delivery** | 75 | A DM to an offline agent is delivered when it reconnects. A send during a hub-server restart isn't lost. Delivery is at-least-once, with dedupe by `meta.id`. Two hub-servers don't double-route (queue group or HA mode). Tests cover offline delivery, router restart and dedupe. | W2-A JetStream | 2 |
| **Persistence** | 85 | Retention policy (TTL/size, configurable, tested). Analytics aggregates in the DB, never loading whole time ranges into memory. Backup/restore documented and tested. | W2-B | 2 |
| **Delegation loop** | 90 | Cancel a running task end to end: `hub-delegate` Ctrl-C, MCP `cancel_task`, Rust and Python workers kill the process group and report `status: cancelled`. `hub-delegate` logs go to stderr, and it accepts `--prompt-file`/stdin. No progress cross-talk between concurrent session turns. Everything covered by e2e tests. | **T3** | 1 |
| **Sessions / waves** | 50 | Wave orchestration runs **in hub-server**: state persisted and resumed after a hub restart; cycle detection; only the assigned worker's events count; liveness TTL marks a dead worker's task failed; the `verify_cmd` result is recorded; fail-fast. The CLI and MCP become thin clients. Worker sessions resume after a worker restart (backend session ids persisted). | **T2** | 1 |
| **Workers** | 80 | One documented entrypoint per worker type, and every advertised type works. The JS workers are either fixed and tested or removed. Every backend is covered by fake-CLI tests: kilo/opencode `--` handling, cancel. `worker_supervisor.py` is under 400 LOC. | **T3** | 1 |
| **Orchestrator surface (MCP)** | 85 | `cancel_task`. A real `check_providers`: live workers, their models, and a cheap liveness ping (the `TODO.md` provider health check). Wave tools wrap the server-side API from T2, with errors surfaced. Plugin install layout tested in CI, simulating `~/.claude/plugins/cache/<mkt>/<name>/<ver>/`. The workflow SKILL is current. | **T4** (+T2 for wave tools) | 1 |
| **Distributed + auth** | 50 | **Identity bound to credentials** (contract §4.1): the router overwrites `meta.from` from the subject, and NATS permissions stop an agent publishing as someone else or reading another agent's inbox. `hub.api` has caller identity and authorization: read scoped to the caller, writes only for admin/orchestrator roles. `hub-admin` mints per-agent creds (user+password or nkey) plus a permission block. Dogfood scripts use per-agent users. A `--require-bound-identity` mode rejects legacy subjects. | **T1** | 1 |
| **Observability** | 75 | The visualizer is split into ES modules with p5 vendored (works offline). Third-party sprites are replaced by a theme manifest and a default theme we can redistribute. A Playwright smoke test covers load, connect with token, and an agent appearing. TUI works against the bound-identity hub. | W2-C | 2 |
| **Portability / install** | 60 | Release workflow: a tag builds linux x86_64/arm64 and macOS arm64 binaries with SHA256SUMS. `install_remote.sh` installs from releases with checksum verification. `no-storage` check in CI. Docs consolidated: historical plans archived, README/AGENTS/CONTRIBUTING current. LICENSE added (**needs Josh's decision**, see §7). | **T5** | 1 |
| *New feature* | — | File-lock broadcast (`TODO.md`): an agent announces "working on file X"; others get notified and can wait until it's released. | W2-D | 2 |

## 3. Wave 1 — five tasks

| ID | Task | Runs on | Why there |
|---|---|---|---|
| **T1** | Identity binding + API authorization + per-agent credentials | Devin | Security-critical and Rust-heavy. Needs a live multi-user NATS config to prove it. Long builds stay off this machine. |
| **T2** | Server-side wave orchestration + session durability | Devin | Rust-heavy, and needs a live stack plus a hub restart to prove resume. |
| **T3** | Delegation & workers to 100%: cancel, stderr logs, prompt-file, cross-talk fix, JS worker decision, supervisor split | native subagent | Uses the local `claude`/`codex` CLIs for smoke tests. Mostly Python plus two small Rust bins. |
| **T4** | MCP surface to 100%: `cancel_task`, real `check_providers`, install-layout CI test, SKILL | native subagent | Python. Can smoke-test with local Claude Code. |
| **T5** | Portability & release: release workflow, install-from-release, `no-storage` CI, docs consolidation | native subagent | Mostly CI and docs work; light local cost. |

**Merge order:** T5 any time. **T1 before T3/T4 merge**, because they must
keep working under `--require-bound-identity`. T2 any time. T4's wave tools
follow T2 (§6).

## 4. Shared contracts (new in iteration 2)

### 4.1 Identity-bound subjects (implemented by T1 everywhere)

| Purpose | Bound subject (new) | Legacy (still accepted unless `--require-bound-identity`) |
|---|---|---|
| Send an envelope | `hub.pub.<identity>.<channel>` | `hub.send.<channel>` |
| Register | `hub.register.<identity>` | `hub.register` |
| Heartbeat | `hub.presence.<identity>` | `hub.presence` |
| Query API | `hub.api.<identity>.<op>` | `hub.api.<op>` |

- `<identity>` is exactly one NATS token matching `[A-Za-z0-9_-]+`. Clients refuse other identities.
- On a bound subject, the router **overwrites** `meta.from` (and the register/presence identity) with the subject's identity. The query API uses it as the caller.
- On a legacy subject, the router routes as before and counts `natshub_unbound_sends_total`.
- With `--require-bound-identity`, the router drops legacy traffic with a warning.
- Delivery subjects are unchanged: `channel.<name>`, `channel.inbox.<id>`, `channel.task.<id>`, and so on.
- **T1 owns the subject strings in every publisher**:
  - Rust `client.rs` and `protocol.rs`
  - `worker_runtime.py`: `publish()`/`announce()` subjects only
  - `worker_events.py`: subjects only
  - `mcp_server/hub_connection.py`: `publish()`/`api_request()` only
  - `nats_connect.py` if needed

  Other tasks must not edit those functions (§6).

### 4.2 Cancel contract (implemented by T3 in workers and `hub-delegate`, by T4 in MCP)

1. The canceller DMs the worker `kind = control`, payload `{"action": "cancel", "task_id": <task envelope id>}`.
2. If the task is running, the worker stops it (it kills the backend's process group), then publishes the terminal result on the task channel per `refocus.md` §6 with `status: "cancelled"`. The status set becomes `done | error | cancelled`.
3. If the task is unknown or already finished, the worker ignores the message (debug log). No reply.
4. `hub-delegate`: the first Ctrl-C sends cancel and waits up to 10s for the `cancelled` result; a second Ctrl-C exits immediately. MCP: `cancel_task(task_id)` returns the terminal snapshot.

### 4.3 Server-side wave API (defined by T2, consumed by T4 and the CLIs)

T2 adds `hub.api` ops, for example `wave.spawn`, `wave.status`, `wave.cancel`,
and documents them in `docs/WAVES.md`. Wave events are published on the
existing `channel.wave.<id>` subjects. T2 also rewrites the **wave functions**
in `mcp_server/` (spawn/status) to call these ops; T4 doesn't touch them.

## 5. Status board

Legend: ⬜ not started · 🟡 in progress · ✅ done (verified + merged) · ⛔ blocked

| ID | Task | Runs on | Branch | Status | Evidence / notes |
|---|---|---|---|---|---|
| T1 | Identity binding + API authz + per-agent creds | Devin | `iter2/t1-identity` | 🟡 | Brief: `.planning/refocus-2/T1-identity.md` |
| T2 | Server-side waves + session durability | Devin | `iter2/t2-waves` | 🟡 | Brief: `.planning/refocus-2/T2-waves.md` |
| T3 | Delegation & workers to 100% | native subagent | `iter2/t3-delegation` | 🟡 | Brief: `.planning/refocus-2/T3-delegation.md` |
| T4 | MCP surface to 100% | native subagent | `iter2/t4-mcp` | 🟡 | Brief: `.planning/refocus-2/T4-mcp.md` |
| T5 | Portability, release, docs | native subagent | `iter2/t5-release` | 🟡 | Brief: `.planning/refocus-2/T5-release.md` |
| W2-A | JetStream durable delivery | wave 2 | — | ⬜ | Blocked on T1 (same router/client code) |
| W2-B | Persistence: retention, analytics pushdown, backup | wave 2 | — | ⬜ | |
| W2-C | Visualizer/observability to 100% | wave 2 | — | ⬜ | |
| W2-D | File-lock broadcast (new feature) | wave 2 | — | ⬜ | |

**Process (unchanged from iteration 1, solo repo):**
- Each task has its own branch, and pushing it runs CI.
- The orchestrator verifies each task independently, then merges locally into `main`. No GitHub PRs unless Josh asks.
- Verification split: CI runs the build/test gates, Devin runs full-environment checks, and local work is review plus targeted runs.
- Every worktree uses its own `./target` with the shared kache cache. Run `make prune` after merges.

## 6. Write-scope ownership

| ID | Owns | Must NOT edit |
|---|---|---|
| **T1** | `src/router.rs` + `src/router/**`; `src/client.rs` + `src/client/**` (send/subscribe subjects, identity validation); `src/protocol.rs`; `src/connect_opts.rs`; `src/query_api.rs` (dispatch/authz; not the handler bodies); new `src/bin/hub_admin.rs`; `config/*.conf*`; `scripts/dogfood_*.sh`; `docs/SECURITY.md`, `docs/OPERATOR_HUB.md`, `docs/JOIN_HUB.md`. **Subject strings only** in `worker_runtime.py`, `worker_events.py`, `mcp_server/hub_connection.py`, `nats_connect.py`. New `tests/identity_*.rs`, `tests/python/test_identity_*.py`. | wave logic, `src/ws_bridge/**`, worker backends, MCP tools/handlers |
| **T2** | New `src/orchestrator/**` (or `src/wave/server.rs`); `src/wave/**`; `src/storage/wave.rs`, `src/storage/session.rs`; new wave/session handlers in `src/query_api/handlers.rs` (new functions only); `src/bin/hub_wave.rs`, `src/bin/hub_session.rs`; hub-server wiring in `src/bin/hub_server.rs` (orchestrator startup only); **wave functions** in `mcp_server/hub_buffers.py`/`hub_handlers.py`/`hub_tools.py`; new `docs/WAVES.md`; `tests/waves*.rs`, `tests/sessions*.rs`. | router, client, auth, non-wave MCP tools |
| **T3** | `src/bin/hub_worker.rs`, `src/bin/hub_delegate.rs`; `worker_runtime.py` and `worker_events.py` (except the §4.1 subject strings); `worker_backends/**`; `*_worker.py`, `worker.js`, `hub_worker.js`, `package.json`; `worker_supervisor.py` (+ split modules); `docs/WORKER_BACKENDS.md`; `tests/e2e_*.rs`, `tests/python/test_worker_*.py`, `tests/python/test_runtime_*.py`, `tests/python/fixtures/**`. | router, client subjects, MCP, storage |
| **T4** | `mcp_server/**` except the §4.1 subject strings and T2's wave functions; `claude-code-plugin/**`, `codex-plugin/**`, `hermes-plugin/**` (via `sync_plugins.sh`); `.claude-plugin/**`, `.agents/**`; `tests/python/test_mcp_*.py`; the plugin install-layout CI job (a new workflow file `.github/workflows/plugins.yml`). | Rust, worker runtime, wave functions |
| **T5** | `.github/workflows/ci.yml`, new `.github/workflows/release.yml`; `packaging/**`, `scripts/install_remote.sh`; `README.md`, `AGENTS.md`, `CONTRIBUTING.md`; `docs/**` except the files listed for T1/T2/T3; moving historical docs to `docs/archive/`; LICENSE scaffolding (§7). | source code |

**Shared-file rule:** `refocus-iteration-2.md`, `refocus.md`, `Cargo.toml` and
`Makefile` belong to the orchestrator. Propose changes in your report instead.
A genuinely required new dependency is fine; flag it.

## 7. Decisions needed from Josh

1. **License.** BSL-1.1, MIT or Apache-2.0? The crate says BSL-1.1, the plugins say MIT, and there's no LICENSE file. T5 prepares everything except the choice.
2. **North star** (carried over from iteration 1): A (a hub for your own multi-machine coding agents, recommended), B (an embeddable crate) or C (a consumer product). It affects wave 2's priorities, mainly how much goes into the visualizer versus JetStream/HA.
