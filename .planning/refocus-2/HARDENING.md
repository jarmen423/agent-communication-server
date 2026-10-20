# Iteration 2 — wave 1 hardening (H1–H3)

Wave 1 (T1–T5) is merged on `iter2/integration` and CI-green, but an
independent review (2026-10-20) found merge-blocking defects that the task
tests and CI missed. Three tasks fix them in parallel; the orchestrator then
adds a production-config end-to-end check, merges to `main`, and starts wave 2.

All line numbers refer to `iter2/integration` at `cd2e86a`.
Read `_COMMON.md` first; its rules apply, with the **build override** below.

## Build override (this machine is low on disk and shared with another CI)

- **Do NOT run `make test` or full `cargo test` locally.** Use `cargo check`
  (`--all-targets`), `cargo fmt --all -- --check`, `make lint`, and **targeted**
  runs only: `scripts/dev/with_stack.sh cargo test --test <name>` and
  `scripts/dev/with_stack.sh .venv/bin/python -m pytest -q tests/python/<file>`.
- Export the bindgen args first (`AGENTS.md` → Build & Run) or use the
  `scripts/dev/*` wrappers.
- **The full gate is GitHub CI on your pushed branch.** Push, then check:
  `curl -s "https://api.github.com/repos/jarmen423/agent-communication-server/actions/runs?branch=<branch>&per_page=5"`.
  `gh` is not authenticated on this machine.
- When done, run `cargo clean` in your worktree to free disk.

## Shared contracts (all three tasks rely on these)

- **C1 — per-identity reply inbox (H1).** Every client uses the request-reply
  inbox prefix `_INBOX.<identity>` (async-nats `ConnectOptions::custom_inbox_prefix`,
  nats-py `inbox_prefix=`). Generated NATS permissions allow each user to subscribe
  `_INBOX.<identity>.>` only. Any user that answers requests gets
  `allow_responses: true` instead of publish `_INBOX.>`.
- **C2 — cancel id (H2 sends, H3 receives).** The orchestrator's cancel DM
  carries `task_id` = **the envelope id of the `session_start` it dispatched**
  for that task, which is what workers register turns under. The orchestrator
  keeps the latest dispatch envelope id per task and persists it on the task
  record (metadata), so a cancel after a hub restart still works.
- **C3 — re-dispatch dedupe (H3).** If a worker receives `session_start` for a
  session id whose turn is **still running**, it does not start a second run.
  It registers the new envelope id as an alias of the running turn, so a cancel
  by either id works.
- **C4 — wave session ids (H2).** Wave task sessions use the id
  `<wave_id>--<task_id>` (one NATS token; validate both parts). The orchestrator
  sends `session_close` when a task reaches a terminal state. Workers treat
  session ids as opaque, and their existing `session_close` handling frees the
  session's state and subscriptions (H3 verifies this).
- **C5 — sender checks (H3).**
  - A worker accepts `cancel` only when `cancel.meta.from` equals `meta.from` of
    the task envelope being cancelled.
  - The orchestrator sends cancels with the same identity it dispatches with
    (H2 verifies this).
  - Delegators accept a terminal result only when `meta.from` is the worker
    they delegated to. That covers `src/client/reply.rs`,
    `mcp_server/hub_primitives.py`, and `hub-delegate`.
  - In permissive mode `meta.from` is self-asserted, so these checks are best
    effort there. In bound mode they are real.
- **C6 — wave dispatch for the Rust worker (H2).** The `session_start` payload
  includes `task_channel` (the wave task channel name), so `hub-worker`
  publishes progress and its terminal result where the orchestrator listens.

## H1 — identity & authorization (`iter2/h1-identity`)

Owns: `src/bin/hub_admin.rs`, `config/*.conf*`, `scripts/dogfood_*.sh`,
`src/connect_opts.rs`, `src/client.rs` (connect only), `src/protocol.rs`,
`src/router.rs` + `src/router/**`, `src/query_api.rs`, `src/query_api/authz.rs`,
`src/query_api/write_authz.rs`, the create paths in `src/storage/wave.rs` and
`src/storage/session.rs`, `src/ws_bridge/**`, `src/bin/hub_server.rs` (bridge
wiring only), `nats_connect.py`, `mcp_server/hub_connection.py` (connect only),
`telegram_bridge.py`, `discord_bridge.py`, `docs/SECURITY.md`,
`docs/OPERATOR_HUB.md`, `docs/JOIN_HUB.md`, `tests/identity_*.rs`,
`tests/python/test_identity_*.py`, `tests/python/test_*bridge*.py`.

1. **`_INBOX.>` is readable and writable by every agent** (`hub_admin.rs:107,133`,
   prod conf example). Implement C1 in the Rust client, in `nats_connect.py`
   (all Python clients connect through it), and in the generated config. Check
   that each responder (hub-server query API, the supervisor's `hub.worker.*`,
   ping responders) still replies.
2. **Create overwrites ownership** (`write_authz.rs:37-45,64`, upserts at
   `storage/wave.rs:187`, `session.rs:116`). `wave.create`, `wave.create_task`
   and `session.create` on an existing id are allowed only for its current
   owner (or admin). Add a regression test where alice tries to take over bob's
   wave and session.
3. **The hub-server user can't subscribe to `channel.wave.>`** (`hub_admin.rs:186-187`,
   prod example). Allow it. Audit what else the orchestrator, router and bridge
   subscribe to under that user.
4. **The legacy API path is fully privileged with `--api-admin` and without
   `--require-bound-identity`** (`authz.rs:118-127,228-229`,
   `query_api.rs:217-219`). When `--api-admin` is set, a legacy caller is a
   non-admin with no ownership.
5. **The WS bridge connects as `"visualizer"`** (`ws_bridge/mod.rs:131`) but stamps
   `--ws-identity`. Connect with the configured identity. Add a test showing
   that a bridge send arrives with `meta.from == ws-identity`.
6. **The Telegram and Discord bridges use legacy `hub.send`**
   (`telegram_bridge.py:162-164`, `discord_bridge.py:211-213`) and send DMs
   without `meta.to`. Use bound subjects and set `meta.to`.
7. **Router hygiene.**
   - Validate `meta.to` with the identity validator and drop or log invalid ones
     (`routing.rs:53-57`).
   - On bound sends, overwrite `meta.channel` from the subject (`router.rs:125-129`).
8. **`hub-admin` hygiene.**
   - Reserve `hub-server`, `hub-orchestrator` (the orchestrator's identity) and
     `visualizer`.
   - Reject extra channels containing `*`, `>` or empty tokens, and reject any
     that overlap `inbox.`, both in `add-agent` and in `render-config`
     (`hub_admin.rs:289-294`).
9. *(low)* `history.query` / `thread.get` filter after `LIMIT`, which gives
    short pages (`authz.rs:250-257`). Over-fetch or push the filter into the query.
10. **Prod-config dogfood**: add `scripts/dogfood_prod_config.sh`.
    - It generates config with `hub-admin`, starts `nats-server` +
      `hub-server --require-bound-identity` on random ports, and starts an echo
      worker as its own user.
    - It asserts inbox isolation: carol can't read bob's inbox, `_INBOX.bob.>`,
      or another agent's API replies.
    - It asserts forged `from` is rewritten.
    - It runs a 2-task wave to completion, and runs a `wave.cancel` on a running
      task, asserting the worker reports `cancelled`.

    The wave and cancel parts depend on H2 and H3. Write them, and mark them
    "expected to pass after integration" if they fail on your branch alone.
11. Update `docs/SECURITY.md` so it says exactly what is and isn't enforced
    after this, including the residual risk that `channel.task.>` and
    `channel.session.>` are readable by every agent.

## H2 — wave orchestrator (`iter2/h2-waves`)

Owns: `src/orchestrator/**`, `src/wave/**`,
`src/storage/wave.rs` and `src/storage/session.rs` (except H1's create
ownership checks), the wave and session handlers in `src/query_api/handlers.rs`,
`src/bin/hub_wave.rs`, `src/bin/hub_session.rs`, the orchestrator startup in
`src/bin/hub_server.rs`, the wave functions in `mcp_server/`, `docs/WAVES.md`,
`tests/wave*.rs`, `tests/session*.rs`.

1. **An unclamped `timeout_secs` panics the orchestrator** (`handlers.rs:95`,
   `engine.rs:324-345`, and again in `adopt_wave` `:231-232` on every restart).
   - Clamp it to 1 s … 7 days at the API boundary and use `checked_add`.
   - On adopt, a stored out-of-range value is clamped, not trusted.
   - Add a regression test for `u64::MAX`.
2. **Cancel never reaches the worker's turn** (`engine.rs:943-950`). Implement C2.
   - Add a live test: dispatch a long task to a Python fake worker, call
     `wave.cancel`, and assert that the worker reports `cancelled` and the
     backend process is gone.
3. **Sessions carry over between waves.** Implement C4. Also make sure
   `session.set_backend_ctx` works for wave tasks, either by creating a session
   record or by documenting the alternative.
4. **The Rust `hub-worker` can't complete a wave task.** Implement C6, with a
   test using `hub-worker --execute`.
5. **Resume races the router.** The orchestrator re-dispatches before the router
   subscribes to `hub.pub.*.>` (`router.rs:371-400`).
   - Start the orchestrator's resume only after the router is subscribed, via a
     readiness signal from the router, or by retrying a dispatch that wasn't
     acknowledged.
   - Re-dispatch reuses the same session id (C3 handles dedupe on the worker
     side).
6. **Cycle detection is bypassed by duplicate dependencies** (`validate.rs:90-112`).
   - Dedupe the dependencies, and only count a node when it is actually removed.
   - Add a test for `["root","root","root"]` alongside an A↔B cycle.
7. **A completed wave can flip to cancelled.** `is_terminal` (`engine.rs:30`)
   doesn't include `completed`, and `wave.cancel` overwrites it (`:378`). Make
   terminal states final.
8. **The assigned worker can bypass the orchestrator.**
   - Ignore completions for tasks that aren't `running` (`engine.rs:440-455`).
   - `wave.update_task_status` from a worker must go through the orchestrator's
     state machine rather than write storage directly (`write_authz.rs:52-61`
     is H1's; change only the handler/engine side, and coordinate via your
     report if the authz rule must change).
9. **Liveness spoofing.**
   - Update `last_seen` only after the sender check (`engine.rs:411`).
   - Prune `last_seen` for identities not referenced by any live wave.
   - Under `--require-bound-identity`, ignore legacy bare `hub.presence`
     (`engine.rs:781-786`), which takes the identity from the payload.
10. *(low)* **`session.set_backend_ctx` loses updates.** It does a
    read-modify-write over the whole record (`handlers.rs:288-295`). Use a field
    merge instead.
11. **Split `engine.rs` (961 LOC)** into focused modules under ~400 LOC each, for
    example `state.rs`, `dispatch.rs`, `liveness.rs`, `commands.rs`, `resume.rs`.
    Do it **first**, in its own commit, with no behaviour change, so the fixes
    land on the new layout. `handlers.rs` (452 LOC): split the wave and session
    handlers out.

## H3 — workers, cancel, MCP (`iter2/h3-workers`)

Owns: `worker_runtime.py`, `worker_events.py` (not subject strings),
`worker_backends/**`, `*_worker.py` (except the bridges), `worker_supervisor.py`,
`src/bin/hub_worker.rs` + `src/bin/hub_worker/**`, `src/bin/hub_delegate.rs`,
`src/client/reply.rs`, `mcp_server/**` except `hub_connection.py` connect and
H2's wave functions, plugin copies (via `scripts/dev/sync_plugins.sh`),
`docs/WORKER_BACKENDS.md`, `tests/e2e_*.rs`, `tests/python/test_worker_*.py`,
`tests/python/test_runtime_*.py`, `tests/python/test_mcp_*.py`,
`tests/python/fixtures/**`.

1. **`run_verify_cmd` can hang forever** (`worker_events.py:153-161`).
   - Don't leave the output unread: stream it, or cap captured output (keep the
     tail for the error message).
   - Run it in its own process group, give it a configurable timeout (default
     10 min), and set `cwd` to the worker's repo or workdir.
   - A cancel during verify kills it and reports `cancelled`, not `done`.
   - Add tests: more than 1 MB of output, a timeout, and a cancel during verify.
2. **There is no cancel or result authorization.** Implement C5 in Python
   (`worker_backends/inbox.py:52-55`), Rust (`hub_worker.rs:273-277`), the
   result side (`client/reply.rs:59`, `mcp_server/hub_primitives.py:34`) and
   `hub-delegate`. Add tests showing that a cancel from a third party is ignored
   and a result from a non-worker is ignored.
3. **Implement C3** (re-dispatch dedupe and alias). Test it.
4. **The Rust `hub-worker` doesn't clean up on SIGTERM** (`hub_worker.rs:146-149`).
   - Handle SIGTERM like Ctrl-C and kill the running child's process group on
     shutdown.
   - Replace shelling out to `kill` (`hub_worker/exec.rs:157-167`) with
     `libc::killpg`. `libc` is already in the dependency tree; if it isn't a
     direct dependency, add it and flag that in your report.
5. **Two terminal results are possible** (`worker_events.py:204-207,233-235`).
   Track that a terminal result was sent and never send a second.
6. **A session message for an unknown session is silent**
   (`worker_runtime.py:276-280`). Publish an `error` on the session channel.
7. **Some turns can't be cancelled.**
   - Register a queued `session_send` turn as soon as it's received
     (`worker_runtime.py:175`).
   - MCP `send_to_session` returns the message id, and `cancel_task` accepts it.
8. **Timeouts leave the worker running.**
   - `hub-delegate` (`hub_delegate.rs:182-192`) and MCP `delegate_task` send a
     cancel when they time out.
   - Cap the timeout arguments in `hub_tools.py:187,235,267` (for example at
     3600 s) via the schema.
9. **The Rust queue.** Bound it (`hub_worker.rs:283`). A cancel matches only
   task envelopes (`:205,279`), never any queued envelope.
10. **MCP eviction** (`hub_buffers.py:101-107`) sets `done` with status `evicted`,
    so waiters return instead of timing out.
11. *(low)*
    - Make the prompt-file and stdin reads in `hub_delegate.rs:92-108` async.
    - `worker_supervisor.py:112` takes the identity from the subject.
12. Verify that `session_close` frees a session's state and subscriptions (C4);
    fix it if not.
