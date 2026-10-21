# H-1a — Split the Rust crate: Apache client crate + BUSL server crate

Read `_COMMON.md` first. Also read `LICENSING.md`, `REUSE.toml`, `docs/HOSTED_PLAN.md`
§4 and §10, and `AGENTS.md`.

Branch: `iter2/h1a-crate-split`. Worktree: `/data/code/acs-wt/h1a-crate-split`.

## Why

- **The license.** The single `nats_hub` crate mixes Apache-2.0 client code and
  BUSL-1.1 server code. Its `license` is `Apache-2.0 AND BUSL-1.1`, and client
  binaries compile in BUSL modules (`LICENSING.md` → "Client code that uses
  BUSL files").
- **The connector.** The `acs` binary (H-1c) and other people's integrations
  need a slim, Apache-only client crate with no SurrealDB/RocksDB.

## Acceptance criteria

1. **A Cargo workspace with (at least) two crates:**
   - **Client crate, Apache-2.0** (name it e.g. `nats-hub-client`, lib
     `nats_hub_client`). It holds `protocol`, `client` (+ `client/**`),
     `connect_opts`, `query_api_client`, `events`, and the client-side
     delivery helpers (`attach_inbox`, the `client_*_stream` helpers,
     `inbox_consumer_name`, `header`, the attach subject constants).
   - It also holds the **wire/record types** the client CLIs and TUI need.
     These are `ApiRequest`/`ApiResponse`, the API subject helpers, the
     storage record/filter types from `storage/types.rs`, `wave::validate`
     and the input types, and `analytics::DataPoint` and friends. Move pure
     data types and pure functions; leave the logic that touches the
     database or the server behind.
   - **No** `surrealdb`/RocksDB dependency.
   - The client CLIs and `hub-tui` (all `hub-*` bins except `hub-server` and
     `hub-admin`) move to the client crate, or to a small Apache bins crate.
     They must build without the storage feature.
   - **Server crate, BUSL-1.1** (keep the name `nats-hub`, lib `nats_hub`).
     It holds router, query API, orchestrator, storage, analytics impl,
     ws_bridge, delivery server side, metrics, `hub-server` and `hub-admin`.
     It depends on the client crate and **re-exports** what it moved
     (`pub use nats_hub_client::*` or per-module re-exports), so existing
     `nats_hub::…` paths in tests and downstream code keep compiling.
2. **No behaviour change.**
   - Binary names, CLI flags, wire format, subjects and env vars stay
     identical.
   - `make build/test/test-bound/test-durable/lint`, the no-storage check (now
     simply building the client crate, plus the server crate's `no-storage`
     feature if it still makes sense; keep CI's job meaningful), the operator
     suite (`scripts/dev/operator_stack.sh … tests/python/test_operator_*.py`)
     and both dogfoods all stay green.
3. **Licensing follows the crates.**
   - Each crate's `Cargo.toml` has one `license` (`Apache-2.0` /
     `BUSL-1.1`).
   - `REUSE.toml` and `LICENSING.md` are updated to the new paths. The
     "client code that uses BUSL files" caveat should disappear. If anything
     client-side still has to use server code, list it.
   - `reuse lint` passes. Run it via `uvx reuse lint` or `pipx run reuse lint`.
     Add it to CI as a cheap step.
   - `tests/python/test_licensing.py` is updated and green.
4. **Release and tooling still work.**
   - `packaging/release/package.sh` and `.github/workflows/release.yml` find
     every `hub-*` binary. The release dry-run test passes.
   - `scripts/dev/*`, `Makefile` targets, `scripts/install_remote.sh` and
     the plugin/bundle paths keep working.
   - The kache/bindgen setup in `scripts/dev/lib.sh` still applies. Each
     worktree has its own `target/`.
5. **Docs.** `AGENTS.md` (code layout, feature flags, common tasks),
   `CONTRIBUTING.md`, `docs/PORTABILITY.md` (embedding the client crate) and
   `LICENSING.md` describe the new layout.

## Write scope

You own the workspace layout: `Cargo.toml` (root, now a workspace) plus the new
crate manifests, `Cargo.lock`, all of `src/**` (moves), `tests/**` (path or
import updates only), `Makefile`, `scripts/dev/**`, `.github/workflows/**`,
`packaging/**`, `REUSE.toml`, `LICENSING.md`, `AGENTS.md`, `CONTRIBUTING.md`,
`docs/PORTABILITY.md`, `docs/RELEASING.md`.

This brief explicitly overrides `_COMMON.md`'s hands-off rule for `Cargo.toml`
and `Makefile`.

Must NOT:
- change behaviour, or touch Python/MCP code. Exception: path changes the
  scripts need, such as the location of `target/debug/hub-*` if that moves.
  Keep binaries at `target/debug/` (a workspace shares one target dir), so
  nothing should move.
- H-1b runs in parallel in `mcp_remote/` and `stub_control_plane/` (Python)
  and may touch `mcp_server/**`. Don't touch those.

## Done means

- All five criteria are met.
- The full gates are green locally: lint, test, test-bound, test-durable,
  no-storage, operator suite, both dogfoods, `reuse lint`.
- The branch is pushed and its CI is green.
- The report lists every moved module (old → new path) and the public
  re-export surface.
