# W3-a: graceful shutdown for hub-server

Read `.planning/refocus-2/_COMMON.md` first if it exists, and
`AGENTS.md`. **Branch:** `iter2/hub-sigterm`. **Worktree:**
`/data/code/acs-wt/hub-sigterm`. Commit there; never push or merge.

## Problem

`src/bin/hub_server.rs` installs no signal handler, so SIGTERM (from
`systemctl stop`, `docker stop`, or hosted tenant suspend and upgrade)
kills the process with exit code 143. The consequences:
- envelopes queued in the bounded storage mirror (`src/router/mirror.rs`)
  and not yet written are lost;
- the embedded SurrealDB/RocksDB is not closed cleanly;
- in `--durable` mode, in-flight JetStream messages may be redelivered.
  That is safe (replay rejection), but noisy.

The hosted control plane stops and restarts tenant hubs routinely, so this
is now on the critical path.

## Acceptance criteria

1. **Signals:** SIGTERM and SIGINT (Ctrl-C) start a graceful shutdown.
   - **Order:** stop taking new work (unsubscribe or stop the routing and
     query-API consumers, stop the WS bridge listener) → finish or ack the
     messages being handled → **drain the mirror queue** with a bounded
     timeout (`--shutdown-timeout`, default 10 s) → stop background tasks
     (retention pruner, orchestrator, delivery listeners) → close storage
     → drain the NATS connection → exit 0.
   - **A second signal**, or the timeout running out, exits non-zero after
     logging how many mirror writes were dropped.
2. **No new channels for routing.** Use a cancellation token passed into
   `ControlPlane::run` (and `run_durable`). Keep the change minimal and
   readable, and keep files under about 400 lines (`hub_server.rs` is
   already 582; split it if you touch it heavily).
3. **Metrics and logs:** log `shutdown requested`, how many queued writes
   were flushed, and the duration. Add a counter for writes dropped at
   shutdown.
4. **Tests (live):**
   - start hub-server against the test stack with a temp DB;
   - publish N envelopes quickly, then send SIGTERM right away;
   - assert exit 0 within the timeout, then start a new hub-server on the
     same DB and assert all N envelopes are in history;
   - cover the default and `--durable` modes;
   - a second-SIGTERM test asserts a fast non-zero exit.
   - Put them in a new Rust integration test file (with_stack provides
     `NATS_URL`).
5. **Deploy files:** `deploy/systemd/nats-hub-server.service` gets
   `KillSignal=SIGTERM` and a `TimeoutStopSec` above the shutdown timeout.
   Document the behaviour in `docs/OPERATOR_HUB.md` (or wherever hub-server
   operations are documented).

## Done means

- `make lint`, `make test`, `make test-bound` and `make test-durable` are
  green; paste the output.
- The new tests fail on current `main`: show that by reverting the handler
  locally once.
- The report lists what changed and anything left over.
