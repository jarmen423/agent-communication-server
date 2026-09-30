# R2 — WS bridge + visualizer transport hardening (remote handoff)

**Branch:** `refocus/r2-ws-bridge` (from `main`; if `refocus/dev-env` isn't merged yet, branch from it)
**Status board:** `refocus.md` §5, row R2. Put evidence in your PR description.

## Read first
1. `refocus.md`: §3 (the "Security issues" list) and §5 (your write scope)
2. `CONTRIBUTING.md` for setup: `make setup && make build && make test` green before you start
3. `src/ws_bridge.rs` (491 LOC, already over the ~400 LOC rule), `src/bin/hub_server.rs`, and the WebSocket code in `visualizer/index.html` (search for `new WebSocket`)

## Problem
`hub-server --ws-addr` serves the visualizer and a WebSocket bridge that can read **all** bus traffic and **spawn workers** (via `ensure_worker` → `worker_supervisor.py`, which starts always-approve agents in the repo). Current state:
1. **Path traversal.** `dir.join(path)` followed by a lexical `starts_with(dir)` (`ws_bridge.rs` ~427) doesn't stop `..` segments. `curl --path-as-is http://127.0.0.1:9191/../../etc/passwd` is expected to leak; confirm that first and add a regression test.
2. **No Origin check.** Any web page the user visits can open `ws://127.0.0.1:9191/ws` (the browser allows cross-origin WebSockets) and read or drive the bus.
3. **No authentication** on the WS endpoint or on command messages.
4. The sender identity is hard-coded as `"josh"` (`ws_bridge.rs` ~265).
5. Each browser tab opens its own NATS connection (~110).

## Deliverables
1. **Split `ws_bridge.rs`** into `src/ws_bridge/{mod.rs, http.rs, ws.rs, commands.rs}` (or similar), each ≤ ~400 LOC. Do this first, with no behavior change and a separate commit.
2. **Static files:** canonicalize both the root and the requested path, reject anything outside the root, and reject any path containing a `..` component or a NUL byte. Add a unit test for the path resolver.
3. **Origin allow-list:** default to `http://<ws-addr>` and `http://localhost:<port>`, plus a `--ws-allow-origin` flag (repeatable). Reject a mismatched Origin with 403 before the WS upgrade.
4. **Token:**
   - `--ws-token <T>`, or env `HUB_WS_TOKEN`. When set, the WS upgrade requires it (for example `?token=` on the WS URL, or a `Sec-WebSocket-Protocol` value).
   - The visualizer reads it from its page URL (`http://…:9191/?token=…`) and passes it along.
   - When unset and binding to a non-loopback address, **refuse to start** unless `--ws-insecure` is given.
   - Print the tokenized URL at startup.
5. **Identity:** replace hard-coded `"josh"` with `--ws-identity` (default `human`) or env `HUB_WS_IDENTITY`.
6. **One shared NATS connection** for the bridge, with per-client subscriptions or a fan-out broadcast channel.
7. **Tests:** `tests/ws_bridge*.rs` for the path resolver, the Origin check, and token acceptance/rejection on upgrade (use an in-process listener on port 0; `tokio-tungstenite` is already a dependency).
8. **Docs:** update `docs/SECURITY.md` (WS bridge threat model and flags) and `docs/VISUALIZER.md` (how to open with a token).

## Constraints
- Don't touch `src/router.rs`, `src/storage/**`, `src/client.rs`, Python files, or plugins.
- Don't edit `README.md`, `AGENTS.md`, `refocus.md` or `Makefile`. Propose snippets in the PR body.
- Avoid new crates if possible. If you do need one, justify it in the PR, and note that `Cargo.toml` is a shared file.
- `make up` must keep working. It binds `127.0.0.1:9191` with no token, which should still be allowed for loopback; log a clear warning.

## Definition of done
- `make lint && make test` green, including the new tests.
- In the PR *Verification* section: real `curl --path-as-is` output before and after; a WS upgrade with a bad Origin → 403; a missing or incorrect token → 401; the visualizer working at `http://127.0.0.1:9191/?token=…`.
