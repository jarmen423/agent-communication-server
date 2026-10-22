# W3-b: client fixes found by the hosted end-to-end run (H2-G)

Read `.planning/refocus-2/_COMMON.md` and `AGENTS.md` first.
**Branch:** `iter2/client-fixes`. **Worktree:** `/data/code/acs-wt/client-fixes`.
Commit there; never push or merge.

H2-G ran every public client unmodified against the real hosted control
plane (private repo). Everything passed except the items below, which are
public-client bugs. The control plane's error codes and behaviour are
listed where they matter.

## Fixes

1. **P-1: Python clients don't survive a tenant suspension.**
   - **Where:** `nats_connect.py`, `_RefreshingCredentialsClient._process_err`.
   - **What happens:** when the server ends a session with
     `maximum account active connections exceeded` (what every client gets
     when the tenant is suspended), `authorization violation` or
     `user authentication revoked`, nats-py treats it as fatal and closes
     the connection.
   - **Fix:** reconnect instead, re-reading the creds file on each attempt,
     which `acs up` keeps fresh. Use backoff with jitter and a cap. Keep
     real permission errors (publish/subscribe permission violations)
     non-fatal and logged as they are now.
   - **Plugin copies:** this file has synced copies; run
     `scripts/dev/sync_plugins.sh`.
   - **Test:** a live test in operator mode (see
     `scripts/dev/operator_stack.sh` and the operator-suite tests).
     Suspend a tenant by pushing an account JWT with connections=0 and a
     `*` revocation, then restore it and re-mint the creds file. A running
     Python client reconnects by itself, and a subscription and a
     request/reply work again.
2. **P-2: workers stay alive but disconnected.**
   - **Where:** `worker_runtime.py`.
   - **Fix:** once the NATS connection is closed for good (closed callback,
     no more reconnects), log it and exit non-zero, so systemd or the
     supervisor restarts the worker.
   - **Test:** with a fake or closed connection, the runtime exits non-zero.
3. **P-3: the `acs` CLI hides the control plane's error codes.**
   - **Where:** `crates/acs/src/api.rs`.
   - **Fix:** add to the error-code allowlist so they print meaningfully:
     `temporarily_unavailable`, `rate_limited`, `quota_exceeded`,
     `key_in_use`, `revocation_list_full`, `unsupported_grant_type`. Today
     they all print as `request_failed`.
4. **P-4: no retry when the tenant isn't ready.**
   - **Where:** `crates/acs/src/api.rs`.
   - **Fix:** retry a 503 that carries `Retry-After` (bounded: at most
     about 60 s in total, honouring the header, capped per attempt). A new
     tenant answers 503 for a few seconds while it is provisioned.
   - **Test:** against a tiny mock HTTP server.
5. **P-5: dev-stub wording on the login line.**
   - **Where:** `crates/acs/src/api.rs` `login` (or wherever the
     approval-URL line is printed).
   - **Fix:** drop the "DEV ONLY control plane" wording. The real control
     plane uses the same flow.
6. **P-6: `acs` has its own reserved-name list.**
   - **Where:** `crates/acs/src/agents.rs` `check_id`. Its hard-coded list
     lacks `lock` and `hub-locks`, which the control plane refuses.
   - **Fix:** use the shared constants from `nats-hub-client` (the same
     source `check_identity` uses), so the two can't drift again.
   - **Test:** add one.
7. **P-7: `hub-server` shutdown hangs when NATS refuses it.**
   - **Where:** `src/bin/hub_server/shutdown.rs` and related code.
   - **What happens:** when NATS refuses the hub (a suspended account),
     the graceful shutdown waits out the full 10 s timeout and exits 1. So
     every suspend takes 10 s and logs an error.
   - **Fix:** skip or shorten the NATS-dependent steps (draining
     consumers, flushing) when the connection is already closed or
     disconnected and can't come back. Still flush the storage mirror, and
     exit 0 if nothing was lost.
   - **Test:** add a live test.

## Done means

- `make lint`, `make test`, `make test-bound`, `make test-durable`,
  `make check-no-storage`, `make test-acs` and the operator suite are
  green; paste the output.
- The new tests fail without their fixes (mutation-check P-1, P-4 and
  P-7).
- The report lists each fix.
