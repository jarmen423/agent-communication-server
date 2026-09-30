# R1 — Unified MCP orchestrator server (remote handoff)

**Branch:** `refocus/r1-mcp` (from `main`; if `refocus/dev-env` isn't merged yet, branch from it)
**Status board:** `refocus.md` §5, row R1. Update your row's *Evidence* column in your PR description, not in `refocus.md`.

## Read first
1. `refocus.md`, especially §3 (gaps), §5 (write scope: you own the plugin dirs only), and **§6 (the reply contract)**
2. `CONTRIBUTING.md` for setup: `make setup && make build && make test` must be green before you start
3. `AGENTS.md` for architecture and conventions
4. The current server: `claude-code-plugin/server/mcp_server.py` (790 LOC). The copies in `codex-plugin/` and `hermes-plugin/` are byte-identical.

## Problem
The MCP server is how an orchestrator (Claude Code, Codex, Hermes) drives the hub. Today it has these problems:
- It is **copy-pasted 3×** with no shared source, which guarantees drift.
- **`delegate_task` is broken.** It takes the first envelope on `channel.task.<id>` as the reply, but workers emit a `started` event first (`mcp_server.py` ~590–600; compare `src/bin/hub_delegate.rs`, which correctly skips `status`/`event`).
- **There is no async loop:** no way to delegate without blocking, poll a task, read your inbox, or wait for a message. `start_session` has no tool that reads worker replies, and `create_wave` never spawns.
- **Auth is ignored.** It uses a bare `nats.connect(NATS_URL)` instead of `nats_connect.connect_nats()`, so it can't join a token or TLS hub.
- **`from` is a per-call argument**, which lets any caller claim any identity. Identity should be stamped from env (`NATS_HUB_IDENTITY`), per the AGENTS.md convention.
- The **Claude Code SessionStart hook** prints `{"context": ...}`. Claude Code expects plain stdout or `hookSpecificOutput.additionalContext`, so check the current Claude Code hooks docs.
- There is **no root `.claude-plugin/marketplace.json`**, so `claude plugin marketplace add jarmen423/agent-communication-server` fails.

## Deliverables
1. **One canonical source** for the server, for example `mcp_server/nats_hub_mcp.py` plus a vendored copy of `nats_connect.py`.
   - Installed plugins must stay **self-contained**: plugin installs copy only the plugin dir, so symlinks out of it break.
   - Add `scripts/dev/sync_plugins.sh`, which copies canonical files into each plugin's `server/`, and a test that fails if the copies drift.
   - Keep each file ≤ ~400 LOC by splitting into modules (connection/buffers, tools, handlers).
2. **Delegation per `refocus.md` §6.**
   - The result is the first `kind=message` whose `meta.reply_to == task_id` **or** `payload.task_id == task_id`.
   - Subscribe before publishing, and handle timeouts.
3. **New tools:**
   - `delegate_async(to, prompt)` → `{task_id, task_channel}`. It subscribes and buffers progress and the result in-process, bounded.
   - `task_status(task_id)` → `{state, last_status, events[-N:], result|error}`
   - `wait_for_task(task_id, timeout)`
   - `read_inbox(limit)` / `wait_for_message(timeout, from?)`. Subscribe to `channel.inbox.<identity>` at startup and keep a bounded ring buffer.
   - `session_replies(session_id, since?)`, or equivalent, so an orchestrator can follow a session.
   - `check_providers()`: stub it, or list alive workers and their `models`/capabilities from heartbeats. This is the TODO.md provider health check, so keep it honest about what it can verify.
4. **Identity** comes from `NATS_HUB_IDENTITY` (required; fail clearly if unset). Remove `from` from tool schemas, or accept it only when it equals the env identity.
5. **Auth** goes through the vendored `connect_nats` (`NATS_URL`, `NATS_TOKEN`, TLS, and creds env).
6. **Plugin surface:**
   - Fix the Claude Code hook output format.
   - Add root `.claude-plugin/marketplace.json` listing `claude-code-plugin`.
   - Keep `.agents/plugins/marketplace.json` working for Codex.
   - In Hermes, delete the dead `handle_nats_tool` and stop re-running `exec_module` (which opens a new NATS connection) on every call.
7. **SKILL.md:** rewrite it as a *workflow* guide (discover → delegate_async → watch → collect; sessions; when to use waves), not a list of tools.
8. **Tests** in `tests/python/test_mcp_*.py`:
   - unit tests for result matching, buffers and identity handling
   - a `live` test under `scripts/dev/with_stack.sh` that runs `delegate_async` → `wait_for_task` against `echo_worker.py`

## Constraints
- Don't edit `README.md`, `AGENTS.md`, `refocus.md`, `requirements*.txt` or `Makefile`. Put the changes you want there in the PR description.
  - `requirements.txt` pins `mcp>=1.10,<2`. If you need MCP SDK 2.x, say so and explain why.
- Don't touch Rust, `worker_*.py`, `worker_backends/**` or `echo_worker.py`.
- Async only (asyncio). Don't swallow exceptions silently.

## Definition of done
- `make lint && make test` green, including your new tests.
- A PR against `main` with a *Verification* section containing the real output of the live test, plus a manual check: register the plugin with `claude --plugin-dir ./claude-code-plugin`, then `delegate_async` → `wait_for_task` to an echo worker started by `make up`.
- A proposed doc snippet for README and AGENTS.md in the PR body.
