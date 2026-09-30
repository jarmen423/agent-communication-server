# T1 — Identity bound to credentials + API authorization + per-agent creds

Branch `iter2/t1-identity` (from `main`). Also read `_COMMON.md`.

## Problem (from iteration 1)
- **Identity is self-asserted.** The router trusts `meta.from`, so any client can send as anyone.
- **Inboxes aren't private.** The default ACL (`config/nats-server.prod.conf.example`) lets every agent subscribe to `channel.inbox.>` and read every DM.
- **The query API has no caller identity or authorization.** `hub.api.*` has no caller identity, so anyone can read all history, including DMs, and call write ops (`wave.update_task_status`, `session.update_status`).
- **Docs overclaim.** `docs/SECURITY.md` claims the router re-scopes senders; it doesn't.

## Deliverables (acceptance criteria: `refocus-iteration-2.md` §2, "Distributed + auth")
1. **Bound subjects**, per contract §4.1:
   - Router subscribes to `hub.pub.*.>`, `hub.register.*`, `hub.presence.*` and `hub.api.*.>`, plus the legacy subjects.
   - Overwrites `meta.from` and the register/presence identity from the subject.
   - Adds `--require-bound-identity` (default off) and the `natshub_unbound_sends_total` metric.
   - Validates identities as `[A-Za-z0-9_-]+` in the router and in the Rust/Python clients.
2. **Clients publish on bound subjects:**
   - Rust `HubClient` (`client.rs`/`protocol.rs`).
   - Python `worker_runtime.py`/`worker_events.py`: subject strings only.
   - `mcp_server/hub_connection.py`: `publish()`/`api_request()`, subject strings only.
   - Run `scripts/dev/sync_plugins.sh` after editing `mcp_server/`.
   - Every existing test must pass both with and without `--require-bound-identity`. Add a `with_stack.sh` variant or env switch so CI can run the bound mode.
3. **Query API authorization** (`src/query_api.rs` dispatch):
   - The caller comes from `hub.api.<identity>.<op>`.
   - Reads are scoped: history and threads only return envelopes where the caller is sender or recipient, or the channel is a broadcast or one of the caller's task or session channels. Define and document the rule precisely.
   - Write ops require an admin role (`--api-admin <id>`, repeatable, or config).
   - Legacy `hub.api.<op>` keeps today's behavior unless `--require-bound-identity` is set, in which case it's rejected.
   - Don't change handler bodies; T2 adds new wave ops there.
4. **`hub-admin` binary:**
   - `hub-admin add-agent <id> [--role worker|orchestrator|admin] [--nkey]` prints a NATS user entry (password or nkey), with a permission block that allows only:
     - publishing `hub.pub.<id>.>`, `hub.register.<id>`, `hub.presence.<id>` and `hub.api.<id>.>`
     - subscribing to `channel.inbox.<id>`, broadcast channels, `_INBOX.>`, and task/session channels according to the documented model
   - Also add `hub-admin render-config`, which writes a full `nats-server.conf` from an agents file.
5. **Dogfood:**
   - `scripts/dogfood_token_auth.sh` and `dogfood_wss_tls.sh` use per-agent users.
   - A new `scripts/dogfood_identity.sh` proves:
     - agent A can't publish as B (the permission violation is visible);
     - A can't read B's inbox;
     - `meta.from` is overwritten on a bound subject;
     - `hub.api` returns only A's DMs to A;
     - a non-admin can't call write ops.
6. **Docs:** rewrite the threat model and flags in `docs/SECURITY.md` truthfully, and update `docs/OPERATOR_HUB.md` and `docs/JOIN_HUB.md` for per-agent creds.
7. **Tests:** `tests/identity_*.rs` (router overwrite, validation, require-bound mode, api scoping and roles) and `tests/python/test_identity_*.py` (Python publishers use bound subjects).

## Out of scope
JetStream (wave 2), wave logic (T2), the WS bridge (already authenticated).
