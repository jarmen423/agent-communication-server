# L2 — Storage + router correctness (local subagent)

**Branch:** `refocus/l2-storage` · **Status board:** `refocus.md` §5 row L2

## Read first
`refocus.md`: §3 and §5 (**your write scope**); `CONTRIBUTING.md`; `AGENTS.md`; `docs/DATABASE_PLAN.md` (for intent; the code is ground truth).

## Problem
- **The schema is never applied.** Every `DEFINE FIELD … AT <table>` in `src/storage/surreal.rs` (~559–616) should be `ON [TABLE] <table>`, and migration errors are swallowed at `debug!` (~620).
  - Sessions and waves store datetimes as *strings*. `parse_or_now` (`storage/wave.rs` ~90, `storage/session.rs` ~60) silently replaces bad values with `Utc::now()`.
  - Session and wave `metadata` is dropped on write.
- **Heartbeats wipe capabilities.** `ControlPlane::handle_presence` calls `registry.register(ident, vec![])` (`router.rs` ~340) instead of `touch()`. `RoutingTable` / `known_agents()` are write-only, and `add_subscriber` appends duplicates.
- **Unbounded queries.**
  - `history.query` has no default limit and can exceed NATS's 1 MB payload.
  - `list_pending` scans every reply in the DB (`surreal.rs` ~461).
  - There are no indexes on `to_identity` / `reply_to` / `channel` / `timestamp`.
  - Analytics loads whole time ranges into memory.
- **`query_api.rs` takes `Arc<SurrealStorage>`** rather than `dyn Storage`, so it bypasses the trait. As a result `cargo check --no-default-features --features no-storage` **fails to compile**.
- **`get_thread` is one level deep**, although the docs describe graph traversal.
- **The router does a publish plus `flush()` per message serially**, and spawns one `tokio::spawn` per envelope for the DB mirror.

## Deliverables
1. **Schema:**
   - Correct the `DEFINE` syntax.
   - Make `migrate()` return an error on any failed statement. `hub-server` should fail fast with a clear message.
   - Store real datetimes for sessions and waves, with read paths tolerant of legacy string rows. Don't use `parse_or_now` fallbacks that hide corruption; log a warning.
   - Persist `metadata`.
   - Add indexes on `envelopes(to_identity)`, `(reply_to)`, `(channel, timestamp)`.
   - Test migrate on a fresh DB **and** on a DB populated by the pre-change code paths (write legacy-shaped rows in a test).
2. **Router:**
   - Heartbeat calls `touch()` and preserves capabilities.
   - Delete or fix the dead `RoutingTable` / `known_agents` (dedupe subscribers).
   - Extract routing into a pure `fn route_subject(&Envelope) -> String` (or similar) and unit test it: DM → `channel.inbox.<to>`, broadcast → `channel.<channel>`, edge cases.
   - Remove the per-message `flush()` from the hot loop (rely on async-nats batching), and bound the DB mirror with a bounded mpsc to one writer task, dropping and counting on overflow via `MetricsCollector`.
   - **Don't** change subjects or routing semantics.
3. **Query API:**
   - Default `limit` 100 and max 1000 on history, thread and pending responses, with a clear error above the max.
   - Switch to `Arc<dyn Storage>`, adding any trait methods needed.
   - Keep `query_api_client.rs` compatible for existing CLIs.
4. **`list_pending`:** a single indexed query (messages to X with no `kind=message` reply whose `reply_to` points at them). Add tests.
5. **`get_thread`:** a recursive or iterative full chain with a depth cap. Test 3+ levels.
6. **`no-storage` compiles:** feature-gate `query_api`, the Surreal analytics imports and the related modules correctly. Add a CI-able command to your report (the orchestrator adds it to CI).

## Constraints
- **Write scope:** `src/storage/**`, `src/router.rs`, `src/query_api.rs`, `src/query_api_client.rs`, `src/analytics/**`, `cfg` gates in `src/lib.rs`, and `tests/{storage_surreal,agent_registry,threads,analytics,router_*}.rs`.
- Don't touch `src/client.rs`, `src/bin/hub_worker.rs`, `src/bin/hub_delegate.rs`, Python or plugins. Another agent owns the reply contract; don't change `reply_to` semantics.
- No `Cargo.toml` changes except feature wiring for `no-storage`; flag them in your report.
- Files ≤ ~400 LOC. `surreal.rs` is 637 now, so split it (for example `storage/envelopes.rs`, `storage/agents.rs`, `storage/schema.rs`).

## Definition of done
`cargo fmt --all -- --check`, `make build`, `make test-rust`, `make test-py` and `cargo check --lib --no-default-features --features no-storage` all green. Paste the real tails in your report.
