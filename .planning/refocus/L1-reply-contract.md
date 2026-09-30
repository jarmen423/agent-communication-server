# L1 — Reply contract + end-to-end delegation harness (local subagent)

**Branch:** `refocus/l1-reply-contract` · **Status board:** `refocus.md` §5 row L1

## Read first
`refocus.md`: §3, §5 (**your write scope**) and **§6 (the contract you implement)**; `CONTRIBUTING.md`; `AGENTS.md`.

## Problem
The core loop "delegate → get the result back" is broken or inconsistent:
- **Rust `hub-worker`** (`src/bin/hub_worker.rs`):
  - It replies with `send_reply` to the sender's *inbox* (~147), but `hub-delegate` listens only on `channel.task.<id>`, so it times out.
  - It writes all of stdin before reading stdout (`wait_with_output`), so a large prompt plus large output deadlocks.
  - It has no timeout or kill.
- **`hub-delegate`** puts the *channel name* into `meta.reply_to` (`.reply_to(&task_channel)`), and storage then creates reply edges to envelopes that don't exist.
- **Python runtime** (`worker_events.py` `execute_with_events`):
  - It sets `reply_to=channel` on the result.
  - Status and event envelopes carry no correlation to the task.
  - `worker_runtime.process_oneshot` silently *skips* DMs without a `task_channel`, so messages from the human bridges (Telegram, Discord) are dropped.

## Deliverables
1. **Rust `hub-worker`** implements §6:
   - If the task payload has `task_channel`: publish `status` (working) and exactly one terminal `message` on that channel. Use `meta.reply_to = task id` and payload `{status, task_id, result, error}`.
   - Otherwise, reply by DM (rule 6).
   - Stream stdin and stdout concurrently.
   - Add `--timeout-secs` (default 600) with a kill, and report it as `status:"error"`.
2. **`hub-delegate`:**
   - Never set `reply_to` to a channel.
   - Match the result per rule 5: `kind=message` and (`reply_to == task_id` or `payload.task_id == task_id`).
   - Keep `--verbose` progress output.
3. **Python runtime** (`worker_events.py`, `worker_runtime.py`):
   - Set `reply_to = task_id` on status, event and result envelopes.
   - Include `task_id` in the result payload.
   - Implement rule 6 for DMs without a `task_channel`, replying by DM with the same result shape, instead of skipping.
   - Session and wave paths must keep working; check `tests/sessions.rs`, `tests/waves.rs` and `tests/events.rs`.
4. **`src/client.rs`:** if useful, add a small helper such as `send_task_result(...)` so Rust callers can't get the contract wrong. Touch only reply helpers.
5. **End-to-end tests** in `tests/e2e_delegation.rs`. These run under `scripts/dev/with_stack.sh`, which provides a real `hub-server`; skip unless `NATS_HUB_TEST_STACK` is set. Use `env!("CARGO_BIN_EXE_hub-worker")` / `CARGO_BIN_EXE_hub-delegate` to spawn the real binaries.
   - (a) `hub-delegate` ↔ Rust `hub-worker --execute rev` (or `cat`) round-trip, checking the exact result.
   - (b) `hub-delegate` ↔ Python `echo_worker.py` (use `.venv/bin/python`; skip if missing).
   - (c) A plain DM without `task_channel` gets a DM reply with `reply_to == id`.
   - (d) Worker timeout produces an `error` result.
   - (e) A 1 MB prompt doesn't deadlock.
   - Also extend `tests/python/test_smoke.py`'s live test, or add `tests/python/test_runtime_contract.py`, to assert `meta.reply_to == task id` on the result.
6. **Fix the known race** in `tests/inbox_routing.rs::test_send_reply_correlation`: subscribe before the reply is sent.
7. **Register immediately.** `worker_runtime.run_worker` sends its first heartbeat only after 30s, so `hub-agents` shows nothing right after `make up` (verified 2026-09-29). Register and heartbeat immediately after subscribing, then keep the interval. Add a live test that the worker appears in `hub-agents` / `agents.list` within 3s.

## Constraints
- Don't touch `src/router.rs`, `src/storage/**`, `src/query_api*.rs`, `worker_backends/**`, `worker_supervisor.py` or plugins.
- Don't edit `README.md`, `AGENTS.md`, `refocus.md`, `Makefile` or `requirements*.txt`; propose any changes in your report.
- Keep files ≤ ~400 LOC. Everything async (`tokio::process`).

## Definition of done
`cargo fmt --all -- --check`, `make build`, `make test-rust` and `make test-py` all green. Paste the real tail of each in your report, plus a manual `make up` + `hub-delegate --to echo-1 --prompt hi` transcript.
