# H-1c — `acs` connector (device login, daemon, MCP install)

Read `_COMMON.md` first. Then read `docs/HOSTED_PLAN.md` §1, §3 (credential
lifecycle, H-0 results) and §4, `docs/OPERATOR_MODE.md`, `docs/REMOTE_MCP.md`,
`stub_control_plane/` (H-1b; its `app.py` docstring reserves the device-code
paths), `crates/client/` (the Apache client crate from H-1a) and
`crates/client/src/connect_opts.rs` + `nats_connect.py` (credential reload).

Branch: `iter2/h1c-acs`. Worktree: `/data/code/acs-wt/h1c-acs`.

## Goal

The Tailscale-like connector. A user installs one binary, runs `acs login`,
and every agent on that machine can join their ACS tenant. It is Rust,
Apache-2.0, and a new workspace crate (`crates/acs`, binary `acs`) built on
`nats-hub-client`.

## Acceptance criteria

1. **Device login (RFC 8628) in the stub control plane.**
   - Add `/device/code`, `/device/token` and a dev "verify" page/endpoint
     that auto-approves the dev user. Tests and the headless flow can also
     approve with a scripted call.
   - Device tokens have the control plane as their audience.
   - `POST /v1/credentials/nats` accepts them **without** the service key,
     but only for identities registered to that device.
   - Add an endpoint to register a device identity (`acs agent add`):
     - name validated with the same rules as `check_identity`, unique in the
       tenant;
     - role `worker` | `orchestrator`, default `orchestrator`.
   - Add an endpoint to revoke a device identity.
   - Add service keys for headless bots: `acs login --key`, a long-lived,
     revocable per-device secret, created through a stub endpoint.
   - Keep everything **DEV ONLY** and labelled as such. Tests for every new
     endpoint, including authz:
     - a device can't mint creds for another device's identity or another
       tenant;
     - revoked identities and revoked devices fail.
2. **`acs` CLI.**
   - **`acs login [--server URL] [--headless] [--key KEY]`.** The browser
     flow prints and opens the URL. `--headless` only prints it. The refresh
     token is stored in the OS keychain, through the `keyring` crate if it
     is acceptable (propose it in your report, since `Cargo.toml` edits are
     yours only for the new crate and workspace members), with a `0600`
     file fallback under `~/.config/acs/`.
   - **`acs logout`** revokes the token and removes local state.
   - **`acs status`** shows server, tenant, device, identities, credential
     expiry and daemon state.
   - **`acs agent add <id> [--role]` and `acs agent rm <id>`.** Generate the
     nkey **locally**; only the public key leaves the machine. Write
     `~/.config/acs/creds/<id>.creds` (`0600`) atomically (write temp, then
     rename).
   - **`acs up [--foreground]` and `acs down`.** The daemon refreshes every
     identity's creds before expiry, with jitter, backoff and clear logs.
     Generate and install a systemd **user** unit on Linux and a launchd
     agent on macOS. `--foreground` is for tests.
   - **`acs mcp install claude|codex|hermes [--identity id]`.** Write the
     MCP/plugin config for that client with `NATS_URL`,
     `NATS_CREDENTIALS_FILE` and `NATS_HUB_IDENTITY`, without clobbering
     unrelated config. Support a `--home` or `ACS_HOME` override for tests.
   - **`acs worker run <backend> --identity id`.** Start an existing Python
     worker or `hub-worker` with the right env. This can be thin.
3. **The two H-0 follow-ups.**
   - (a) Derive the reply-inbox identity from the creds file's JWT `name`
     claim when no explicit inbox identity is set, in both `connect_opts.rs`
     and `nats_connect.py`. Then the tenant hub-server no longer needs the
     `NATS_USER=hub-server` hint. Update `docs/OPERATOR_MODE.md` and
     `operator_stack.sh` if they set it.
   - (b) In `nats_connect.py`, replace the nats-py private-method hook
     (`_process_err`) with a public mechanism if nats-py has one. If not,
     keep it, pin nats-py exactly in `requirements.txt`, and say why in a
     comment.
4. **End-to-end test, in CI on the operator stack.**
   - Bring up: stub control plane + operator stack + a durable, bound tenant
     hub-server + an echo worker.
   - Run `acs login --headless` with a scripted approve, then
     `acs agent add me`, then `acs mcp install claude --home <tmp>`.
   - Start the stdio MCP server exactly as the generated config says, then
     run `delegate_task` to the echo worker and get the result.
   - Run `acs up --foreground` with a short NATS TTL (e.g. 60 s). A
     connected `HubClient` keeps working across a refresh, and an
     unrefreshed copy is rejected.
   - Run `acs agent rm` and confirm the credential no longer refreshes.
5. **Packaging and docs.**
   - The release workflow ships `acs` (check `packaging/release/package.sh`
     picks it up; the glob may be `hub-*` only).
   - `scripts/install_remote.sh` can install it.
   - Add a new `docs/ACS_CONNECTOR.md`, update `docs/HOSTED_PLAN.md` §4
     (what was built) and the `AGENTS.md` CLI reference and layout.
   - `REUSE.toml`: the new crate is Apache-2.0 (check that the map covers
     it), and `uvx reuse lint` is green.

## Write scope

Owns:
- new `crates/acs/**`; workspace `members` in the root `Cargo.toml` (only that line), `Cargo.lock`
- `crates/client/src/connect_opts.rs` (follow-up a), `nats_connect.py` + `mcp_server/nats_connect.py` and its synced copies (follow-ups a and b; run `sync_plugins.sh`), `requirements.txt` (nats-py pin)
- `stub_control_plane/**` (device flow, device identities, service keys)
- `scripts/dev/operator_stack.sh` (only if follow-up a lets it drop the hint), `packaging/release/**`, `scripts/install_remote.sh`, `.github/workflows/{ci,release}.yml` (small steps)
- new tests (`crates/acs/tests/**`, `tests/python/test_acs_*.py`), docs listed above, `REUSE.toml` (if needed)

Must NOT edit:
- server code (`src/**`), `mcp_remote/**` beyond what the stub API change needs (keep the H-1b e2e green), `visualizer/**`
- W2-D (file locks) and W2-C (visualizer) are running in parallel.

## Done means

- All five criteria are met.
- `make lint`, `make test`, `make test-bound`, `make test-durable` and `make check-no-storage` are green; so are the operator suite, `make test-remote-mcp`, your e2e and `uvx reuse lint`.
- The branch is pushed and its CI is green.
- The report includes the real e2e transcript.
