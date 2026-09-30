# T2 — Server-side wave orchestration + session durability

Branch `iter2/t2-waves` (from `main`). Also read `_COMMON.md`.

## Problem (from iteration 1)
- **Wave orchestration lives in the client.** The spawn loop runs in the `hub-wave` CLI process (`src/bin/hub_wave.rs`), and there's a second copy in the MCP server (`mcp_server/hub_buffers.py` `spawn_wave`/`_run_wave`). If the process exits, the wave stays "running" forever.
- **No cycle detection.** A→B plus B→A passes validation and then hangs until timeout.
- **Anyone can finish a task.** Anyone can publish a `completed` event on a task channel; the sender is never checked against the assigned worker.
- **No liveness.** A task assigned to a dead worker hangs until the global timeout.
- **`verify_cmd` isn't enforced.** It is passed to the worker and never recorded or enforced by the hub.
- **Sessions don't survive worker restarts.** Worker session state is memory-only, so after a worker restart `session_send` is dropped, even for backends that can resume: Claude `--resume`, Codex `exec resume`, Cursor `agent_id`.
- **Duplicate code.** `wave::spawn_wave` duplicates `hub_wave.rs`.

## Deliverables (acceptance criteria: §2, "Sessions / waves")
1. **Orchestrator in hub-server** (new `src/orchestrator/` or `src/wave/server.rs`):
   - Owns wave state machines, persisted through `Storage` (wave + wave_tasks), and resumes running waves after a hub-server restart.
   - One implementation: delete the CLI/lib duplicate.
2. **Validation and enforcement:**
   - Cycle detection at create time.
   - Only envelopes from a task's assigned worker (`meta.from`; T1 makes that trustworthy) can change its state.
   - Liveness TTL (configurable) marks a dead worker's task `failed`.
   - Fail-fast.
   - The `verify_cmd` result (from the worker's `milestone verify_passed`/`error`) is recorded on the task.
3. **API:**
   - New `hub.api` ops, as new functions in `src/query_api/handlers.rs`: `wave.spawn`, `wave.status`, `wave.cancel` and whatever else you need.
   - Documented in a new `docs/WAVES.md`.
   - Progress published on `channel.wave.<id>` (existing subjects).
4. **Thin clients:**
   - `hub-wave spawn/status/cancel` call the API and exit; `hub-watch --wave` still streams.
   - Rewrite the wave functions in `mcp_server/` (`spawn_wave`, wave status) to call the API. Delete the in-process loop and surface errors. Run `scripts/dev/sync_plugins.sh`.
5. **Session durability:**
   - Persist the backend session id (e.g. `claude_session_id`, `codex thread id`, cursor `agent_id`) with the session record through the API, so a restarted worker can resume a session it no longer holds in memory.
   - Coordinate through the API. **Don't edit `worker_runtime.py`**, which belongs to T3.
   - Add the storage/API side plus a documented hook (`session.get` returns `backend_ctx`). T3's runtime will call it; note the exact call in your report.
6. **Tests:**
   - Cycle rejection.
   - Resume after a hub-server restart (live, via `with_stack`-style start/stop).
   - A foreign sender is ignored.
   - A dead worker leads to task failure.
   - Fail-fast.
   - The MCP wave tools against the API.

## Out of scope
Identity binding (T1). Rely on `meta.from` as-is; after T1 it's trustworthy.
