# W2-B — Persistence: retention, analytics pushdown, backup/restore

Read `_COMMON.md` first. Acceptance criteria come from `refocus-iteration-2.md`
§2, "Persistence". Read `docs/DATABASE_PLAN.md`, `src/storage/**`,
`src/analytics/**`, `src/query_api/stats.rs`, and `docs/HOSTED_PLAN.md` §5
and §7: every hosted tenant gets one hub-server with embedded SurrealDB on a
volume.

Branch: `iter2/w2b-persistence`. Worktree: `/data/code/acs-wt/w2b-persistence`.

## Why

The embedded SurrealDB (RocksDB) history grows without bound. Analytics may
pull whole time ranges into memory. There is no supported backup or restore.
For hosted tenants this is the difference between a hub you can operate and
one you can't.

## Acceptance criteria

1. **Retention policy.**
   - Configurable by age (e.g. `--retention-days`) and/or size (max
     envelopes), off by default.
   - Enforced by a background pruner in hub-server that deletes in bounded
     batches and never blocks the router or the mirror. It exposes a counter
     on `/metrics`.
   - Never deletes non-terminal waves or sessions, or anything a running wave
     or session still references.
   - Terminal waves and sessions get their own (longer) retention.
   - Tests cover age, size, the "keep live state" rule and batch bounds.
2. **Analytics aggregate in the database.**
   - Every `Analytics` method (`src/analytics/surreal.rs`) computes its
     aggregate in SurrealQL (GROUP BY, count, math::*), not by loading rows
     into Rust.
   - Memory is bounded regardless of the range. Add a test with a large
     seeded dataset that would fail or blow a limit under the old approach,
     or argue precisely why a test like that isn't meaningful and assert the
     query shape instead.
   - `hub-stats` output is unchanged. Keep the existing analytics tests
     green.
3. **Backup and restore, documented and tested.**
   - A consistent backup of a **running** hub-server, plus restore into a
     fresh DB path.
   - SurrealDB v2's export/import is a candidate. You choose: an admin-only
     query-API op, a `hub-server` subcommand, or both. Admin-only means it
     goes through the existing `--api-admin` checks.
   - A round-trip test covers envelopes, agents, sessions, waves and wave
     tasks.
   - Documented in `docs/DATABASE_PLAN.md` and the backup section of
     `docs/OPERATOR_HUB.md`, with a systemd timer example under `deploy/`.
   - State what's lost between backups, and that JetStream (`--durable`) is a
     delivery buffer, not the archive.

## Write scope

Owns:
- `src/storage/**`, `src/analytics/**`, `src/query_api/stats.rs`
- New handler file(s) under `src/query_api/handlers/`, plus the dispatch lines in `src/query_api.rs` and `src/query_api/authz.rs` / `write_authz.rs` (admin-only checks)
- `src/bin/hub_server.rs` (flags and wiring), `src/bin/hub_stats.rs`
- `tests/storage_*.rs`, `tests/analytics.rs`, new `tests/retention*.rs`, `tests/backup*.rs`
- `docs/DATABASE_PLAN.md`, the backup section of `docs/OPERATOR_HUB.md`, `deploy/**`

Must NOT edit:
- router, `src/delivery/**`, orchestrator, client code, `hub_admin`, MCP, Python workers
- `Cargo.toml` and `Makefile`: propose any changes instead.
- H-0 (operator mode) runs in parallel and owns `hub_admin`, `connect_opts`, the dogfood scripts and `docs/HOSTED_PLAN.md`.

## Done means

- All three criteria are met with tests.
- `make lint` and `make test` are green, and `make test-bound` and `make test-durable` stay green.
- The no-storage check passes.
- A live backup → restore on a real `make up` stack, with its real output in the report.
- The branch is pushed and its CI is green.
