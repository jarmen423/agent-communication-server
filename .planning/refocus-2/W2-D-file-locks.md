# W2-D — File-lock broadcast

Read `_COMMON.md` first. Then read `refocus-iteration-2.md` §2 ("New feature")
and §4, `docs/SECURITY.md` (bound identity, owner-scoped writes),
`docs/WAVES.md`, `docs/DELIVERY.md`, and the layout in `AGENTS.md`. Since H-1a
the client library lives in `crates/client` (Apache-2.0) and server code in
`src/` (BUSL-1.1).

Branch: `iter2/w2d-file-locks`. Worktree: `/data/code/acs-wt/w2d-file-locks`.

## Goal

When several agents edit one repo, an agent announces "I'm working on these
files". Others are notified, can see who holds what, and can wait until the
files are released. This is advisory coordination, not filesystem locking.

## Acceptance criteria

1. **Model.** A lock covers a `scope` (a repo or project key, a free string,
   e.g. a git remote or repo path; default: the caller's choice) and a set of
   `paths` (repo-relative files or directory prefixes ending in `/`). Each
   lock has:
   - a holder identity (from the bound subject, never the payload);
   - an optional `reason` / `task_id`;
   - a TTL (default 15 min, max 24 h), renewable.

   Two locks **conflict** when they share a scope and any of their paths
   overlap (equal, or one is a directory prefix of the other).
2. **Server side (hub-server, persisted).**
   - Ops: `lock.acquire`, `lock.renew`, `lock.release`, `lock.list`,
     `lock.get`.
   - `acquire` is all-or-nothing. On a conflict it returns the conflicting
     locks; it never partially grants.
   - Only the holder renews or releases. Admins (`--api-admin`) can force a
     release.
   - Locks expire at their TTL, and also expire when the holder's presence
     is lost. Use the same liveness TTL idea as waves; reuse the presence
     feed.
   - State survives a hub-server restart (storage, behind the `Storage`
     trait, with a migration).
   - Concurrent acquires of overlapping paths: exactly one wins. Test this
     with many concurrent requests.
3. **Notifications.**
   - Every acquire, release and expiry is broadcast on a lock channel
     (e.g. `channel.lock.<scope-hash>`) as a `MessageKind::Event` with
     `{event_type: lock.acquired|lock.released|lock.expired, …}`.
   - Waiters can block until all their paths are free: client-side wait
     with a timeout, driven by the events plus a re-check, so a missed event
     can't hang a waiter.
   - Works in default, `--require-bound-identity` and `--durable` modes. In
     bound mode the channel must be readable through `hub-admin`'s generated
     permissions; extend `acl.rs` minimally (shared policy, so
     `permission-template` and config mode stay in sync, and the frozen
     policy test is updated).
4. **Surfaces.**
   - Rust client: `HubClient`/`ApiClient` methods in `crates/client`.
   - New `hub-lock` CLI (`acquire/renew/release/list/wait`), in the client
     crate.
   - MCP tools in `mcp_server/` (`lock_files`, `release_files`,
     `wait_for_files`, `list_locks`, with `readOnlyHint` where it applies),
     then `scripts/dev/sync_plugins.sh`.
   - Update the nats-hub SKILL.md: when to lock (before editing shared
     files in a wave) and how.
   - Optional: the wave orchestrator acquires locks for each task's
     `write_scope` on dispatch and releases them on terminal state. Do this
     only if it stays small and is fully tested. Otherwise propose it.
5. **Tests and docs.**
   - Rust unit and integration tests: conflict rules, concurrency, TTL,
     presence expiry, restart, authz (non-holder can't release; admin can).
   - A Python MCP test.
   - A live two-agent demo in the report: A locks, B waits, A releases, B
     proceeds.
   - New `docs/FILE_LOCKS.md`, plus an `AGENTS.md` CLI reference row.

## Write scope

Owns:
- new `src/locks/**` (server)
- new storage files under `src/storage/` (+ the schema migration lines and the `Storage` trait methods)
- query-API handlers and dispatch for `lock.*`, plus `authz.rs`/`write_authz.rs` entries
- `src/bin/hub_server.rs` (wiring only)
- `crates/client/src/**`: new lock methods and the `hub-lock` bin, plus its `[[bin]]` in `crates/client/Cargo.toml`
- `src/bin/hub_admin/acl.rs` (lock channel permission)
- `mcp_server/**` (lock tools only) + plugin sync
- new tests; `docs/FILE_LOCKS.md`, `AGENTS.md`, the SKILL.md

Must NOT edit:
- `src/delivery/**`, the router's routing logic, `visualizer/**`, `mcp_remote/**`, `stub_control_plane/**`
- H-1b and W2-C run in parallel. H-1b also edits `mcp_server/**`, so keep your MCP changes to new files plus small registration edits, to make the merge easy.

## Done means

- All criteria are met.
- `make lint`, `make test`, `make test-bound` and `make test-durable` are green, along with the operator suite.
- The branch is pushed and its CI is green.
- The live demo output is in the report.
