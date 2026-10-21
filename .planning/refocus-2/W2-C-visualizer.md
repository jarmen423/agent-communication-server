# W2-C — Visualizer and observability to 100%

Read `_COMMON.md` first. Then read `refocus-iteration-2.md` §2
("Observability"), `docs/VISUALIZER.md`, `docs/pets.md`, `LICENSING.md` (the
third-party sprites section), `REUSE.toml`, and `src/ws_bridge/**` (how the
visualizer connects). The TUI lives in `crates/client/src/tui/` since H-1a.

Branch: `iter2/w2c-visualizer`. Worktree: `/data/code/acs-wt/w2c-visualizer`.

## Acceptance criteria

1. **ES modules, works offline.**
   - Split `visualizer/index.html` (about 3,400 lines with inline JS) into
     ES modules: scene/render, WS connection, state/model, UI panels,
     themes.
   - Keep each file under about 400 lines.
   - Vendor p5.js into `visualizer/vendor/` at a pinned version with a
     checksum recorded, instead of the CDN `<script>`. p5.js is
     LGPL-2.1-or-later: record it in `REUSE.toml` (`LICENSES/` gets
     `LGPL-2.1-or-later.txt`) and in `LICENSING.md`'s third-party section.
   - No other network fetches at runtime.
   - Everything the visualizer did before still works: agents, messages,
     tasks, waves, sessions, "ensure worker", token auth.
2. **Replace the third-party sprites.**
   - Delete `visualizer/pets/**`: Petdex sprites, some depicting others'
     characters.
   - Delete their `LicenseRef-Petdex-Unverified` entries in `REUSE.toml`,
     `LICENSES/` and `LICENSING.md`.
   - Add a theme manifest (`visualizer/themes/<name>/theme.json`: sprite
     sheets, animations, palette) and an **original default theme** we can
     redistribute under Apache-2.0 or BUSL-1.1 like the rest of the
     visualizer. Procedurally generated or hand-made pixel art is fine.
   - Update `docs/pets.md` (or replace it with `docs/THEMES.md`) to say how
     to add a theme.
   - `uvx reuse lint` stays green.
3. **Playwright smoke test**, run in CI:
   - Start a throwaway stack: `scripts/dev/with_stack.sh`, or a hub-server
     with `--ws-addr` and `--static-dir visualizer/` plus a WS token.
   - Load the page, connect with the token, start an echo worker, and
     assert that the agent appears and a delegated message animates or
     shows in the feed.
   - Run headless Chromium. Put it under `tests/e2e_visualizer/`, add a small
     CI step, and pin versions (`package.json` + lockfile in that dir, or
     Python Playwright in `requirements-dev.txt`, whichever is lighter).
4. **TUI against a bound hub.**
   - `hub-tui` works against `hub-server --require-bound-identity
     --api-admin …` with its identity from `NATS_HUB_IDENTITY` (bound API
     subjects).
   - Add a test, or at least a scripted check that runs in CI.
   - Fix whatever doesn't work.
5. **Docs.** Update `docs/VISUALIZER.md` with the module layout, offline
   use, themes and the smoke test.

## Write scope

Owns:
- `visualizer/**`, new `tests/e2e_visualizer/**`
- `.github/workflows/ci.yml`: one small step
- `REUSE.toml`, `LICENSES/`, `LICENSING.md`: only the sprite/p5 entries
- `docs/VISUALIZER.md`, `docs/pets.md` / `docs/THEMES.md`
- `crates/client/src/tui/**` and `crates/client/src/bin/hub_tui.rs`: bound-mode fixes only
- `src/ws_bridge/**`: only if the smoke test needs it; keep changes minimal and tested

Must NOT edit:
- `mcp_server/**`, `mcp_remote/**`, `stub_control_plane/**`
- storage, router, delivery, locks
- W2-D (file locks) and H-1b (remote MCP) run in parallel.

## Done means

- All criteria are met.
- `make lint` and `make test` are green, `reuse lint` is green, and the Playwright smoke test passes locally and in CI.
- The report includes a screenshot of the default theme, saved under `tests/e2e_visualizer/artifacts/` and gitignored.
