# T5 — Portability, release, docs

Branch `iter2/t5-release` (from `main`). Also read `_COMMON.md`.

## Deliverables (acceptance criteria: §2, "Portability / install")
1. **`.github/workflows/release.yml`:**
   - Triggered by a `v*` tag.
   - Builds release binaries (`hub-server`, `hub-*` CLIs, `hub-tui`) for linux x86_64, linux arm64 (native runner or cross) and macOS arm64.
   - Packages the tarballs, generates `SHA256SUMS`, and attaches them to a GitHub Release.
   - Test it with a `workflow_dispatch` dry run that skips publishing. Do **not** create a real release or tag.
2. **Install from release:** `scripts/install_remote.sh` / `packaging/remote/install.sh` download from releases with checksum verification, falling back to building from source. The default URL is TLS or documented. Test with a local fake release directory.
3. **CI:** add `cargo check --lib --no-default-features --features no-storage`, and cache what's worth caching.
4. **Docs consolidation:**
   - Move historical plans (`docs/PHASE3_PLAN.md`, `PHASE4_PLAN.md`, `TUI_PLAN.md`, `DB_ACCESS_PROBLEM.md`, `docs/operator-notes/*`, `.planning/execution/**`, `.planning/TUI_HARDENING_HANDOFF.md`) to `docs/archive/`, with an index.
   - Rewrite `README.md` as the pitch plus a 5-minute quick start, with links.
   - Make `AGENTS.md`'s code layout and test counts accurate for current `main`: the new storage/router/query_api modules, the tests, `mcp_server/`, `claude_worker.py`/`codex_worker.py`.
   - `CONTRIBUTING.md` current.
   - Fix stale paths and statements.
   - Don't edit `docs/SECURITY.md`, `OPERATOR_HUB.md`, `JOIN_HUB.md` (T1), `docs/WAVES.md` (T2) or `docs/WORKER_BACKENDS.md` (T3).
5. **License scaffolding:** don't choose a license. Josh decides between BSL-1.1, MIT and Apache-2.0. Prepare a `LICENSE.md` note listing where the license is declared today (`Cargo.toml`, plugin manifests, README) and a script or checklist that switches every declaration to the chosen license in one step. **Leave the existing declarations unchanged.** They disagree today; that's the decision Josh is making.

## Must not touch
Source code (`src/**`, Python modules), except trivial doc comments.
