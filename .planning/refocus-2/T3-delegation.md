# T3 — Delegation loop & workers to 100%

Branch `iter2/t3-delegation` (from `main`). Also read `_COMMON.md`.

## Deliverables (acceptance criteria: §2, "Delegation loop" + "Workers")
1. **Cancel, per contract §4.2:**
   - Rust `hub-worker` kills the running command's process group and publishes `status: "cancelled"`.
   - The Python runtime routes `kind=control {action: cancel}` to the running task; every backend in `worker_backends/**` supports cancel (process-group kill for CLIs, `session/cancel` for ACP).
   - `hub-delegate`: the first Ctrl-C sends cancel and waits up to 10s for the result; a second Ctrl-C exits.
   - e2e tests on both the Rust and Python paths.
2. **`hub-delegate`:**
   - All logs go to **stderr**, so stdout carries only the result. Today, with `RUST_LOG` set, INFO logs pollute stdout and break 2 e2e tests.
   - Add `--prompt-file <path>` and `--prompt -`, which reads stdin; the command-line argument limit is 128 KiB.
   - Test both.
3. **Progress cross-talk:** the progress handler is attached to a shared backend, so concurrent session turns can see each other's progress. Make it per-turn, and add a test with two concurrent turns.
4. **JS workers:**
   - Decide, with evidence, whether to fix `hub_worker.js` + `worker.js` or remove them.
   - `hub_worker.js` advertises `agy|hermes|cursor` but only `cline` works.
   - If you keep them: one entrypoint, the reply contract, auth via env, and a test with a fake Cline SDK or `node --test`.
   - If you remove them: update the docs and the supervisor.
5. **Worker entrypoints:**
   - Every type the supervisor advertises actually starts.
   - Fake-CLI tests for the kilo/opencode `--` prompt handling.
   - Split `worker_supervisor.py` (403 LOC) to under 400.
6. **Session resume:** once T2 lands, the runtime can fetch `backend_ctx` for unknown sessions through the API. Stub the call behind a feature check and note the exact integration. Don't block on T2.
7. **Real smoke tests** (cheap): one `claude` and one `codex` round trip through the hub with prompt "Reply with exactly: pong", plus one cancel of a long-running fake. Paste the transcripts.

## Must not touch
The §4.1 subject strings (`publish()`/`announce()` subjects belong to T1), MCP, router, storage.
