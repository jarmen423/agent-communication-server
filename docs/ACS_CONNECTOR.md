# ACS device connector

`acs` is the Apache-2.0 Rust connector in `crates/acs`, built on
`nats-hub-client`. It manages a device's credentials; agents connect directly
to the tenant's NATS account. The current control plane is **DEV ONLY**:
verification auto-approves the fixed dev user. Do not expose it with real data.

## Install and sign in

Release tarballs ship `acs` beside `hub-*`. `scripts/install_remote.sh <HUB>`
installs them into `~/.local/share/nats-hub/bin/`; add that directory to PATH.
From source run `make setup && make build`, or use the remote installer
with `--from-source --with-acs`. The connector itself links no
server or database code.

```bash
acs login --server https://control-plane.example
# Or print a URL to open on another device:
acs login --server https://control-plane.example --headless
acs agent add me                         # orchestrator by default
acs agent add laptop-worker --role worker
acs up                                  # install/start a user service
acs status
acs worker run echo --identity laptop-worker
acs mcp install claude --identity me
```

Only HTTPS control-plane URLs are accepted, except HTTP on loopback for the
stub. `ACS_SERVER` supplies the default server. The stub's `--nats-url` is the
NATS URL returned at login; pass the operator stack's private `NATS_URL` when
running a local demo. HTTP redirects aren't followed with bearer credentials.

`acs login --key KEY` exchanges a long-lived, revocable **per-device** secret.
Prefer `ACS_SERVICE_KEY` to avoid shell-history exposure. The key is exchanged
for the usual short-lived access token and rotating refresh token; the CLI
does not retain the service key. No NATS user seed is sent to the issuer.

## Local state and renewal

Default state is `~/.config/acs/`; `ACS_HOME` overrides that directory for
isolated installs/tests. Refresh tokens use the OS keychain (`keyring`:
macOS Keychain, Linux Secret Service). When unavailable, an atomic `0600`
`refresh.token` file is used with a clear message. `ACS_NO_KEYRING=1` forces
that fallback for headless tests. State and credential files are also `0600`,
inside the private `0700` state directory. Complete files are synced and
renamed over the old file; no partial seed/JWT pair is exposed. Separate
operation and daemon locks serialize refresh-token rotation and identity edits.

`creds/<identity>.creds` contains that identity's locally generated nkey seed
and short-lived NATS JWT. The public key is bound at registration. Renewal
keeps the same seed/public key; only the signed JWT changes. The daemon
refreshes with a randomized 15–25 second margin, retries failures with capped
exponential backoff and jitter, and logs identity/expiry without secrets.
Rust and Python clients reread the credentials on reconnect, including the
reconnect triggered by actual JWT expiry.

`acs up` writes `~/.config/systemd/user/acs.service` on Linux and
`~/Library/LaunchAgents/com.agentmemorylabs.acs.plist` on macOS. The service
uses the running executable's absolute path and explicit `ACS_HOME`.
`acs up --foreground` runs without a service manager, useful on servers,
containers and in tests. `acs down` stops the daemon. `acs agent rm <id>`
revokes renewal, removes its local state/credential, and is serialized with
the daemon. `acs logout` stops the daemon, revokes the device (including its
service keys), and removes local credentials and tokens. Logout requires a
reachable control plane so it never claims remote revocation succeeded offline.
MCP configuration entries remain; remove them using your client's MCP UI/CLI.

The DEV ONLY stub does not push NATS account revocations. Removing an identity
or device immediately stops new issuance/renewal; already issued NATS JWTs
remain valid until expiry. Production must push resolver revocations as in
[OPERATOR_MODE.md](OPERATOR_MODE.md).

## MCP and workers

The installer merges only `nats-hub`; unrelated client settings/servers remain.
It writes `command`, `args`, and the three environment variables `NATS_URL`,
`NATS_CREDENTIALS_FILE`, `NATS_HUB_IDENTITY`. It never puts a token/seed into
client configuration. Choose `--identity` when more than one exists.

- Claude Code: `~/.claude.json`, `mcpServers.nats-hub` ([user scope](https://code.claude.com/docs/en/mcp)).
- Codex: `~/.codex/config.toml`, `[mcp_servers.nats-hub]` ([MCP configuration](https://developers.openai.com/codex/mcp)).
- Hermes: `~/.hermes/config.yaml`, `mcp_servers.nats-hub` ([configuration reference](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/reference/mcp-config-reference.md)).

`--home <dir>` changes the **client home**, while `ACS_HOME` changes connector
state. The config uses absolute credential/runtime paths, so tests can launch
exactly what it generates. TOML comments are retained; YAML/JSON formatting
may be normalized. Invalid config types fail without overwriting the file.

In a checkout, the installer discovers `mcp_server/nats_hub_mcp.py` and the
repo's venv. Set `ACS_MCP_SERVER`/`--server` and `ACS_PYTHON`/`--python` to use
an existing runtime. A standalone binary embeds the canonical Python modules,
writes them under `ACS_HOME/mcp/server`, and creates a private venv with pinned
nats-py and the MCP dependencies. This first install needs Python 3.10+ with
venv/pip and network access. It does not embed a Python interpreter.

`acs worker run <backend> --identity <id> -- <extra args>` wraps an existing
Python worker; set `ACS_WORKER_DIR` to the worker bundle/checkout directory and
optionally `ACS_PYTHON`. `hub-worker` uses the sibling binary or PATH and
accepts its usual extra flags, for example `-- --execute cat`. The wrapper
sets the same NATS environment as MCP and transfers signals/exit status to
the worker. Worker code and model CLIs still need to be installed separately.

## DEV ONLY endpoints and test

The existing OAuth/MCP routes remain unchanged. Device endpoints are:

- `POST /device/code`: form-encoded public `client_id=acs`; returns RFC 8628
  code, user code, verification URL, expiry and interval.
- `GET|POST /device/verify`: auto-approve a user code; scripted POST accepts `user_code`.
- `POST /device/token`: form-encoded device-code grant (public `client_id=acs`
  required), rotating `refresh_token`, or `service_key` exchange.
- `GET|DELETE /v1/device`: inspect/revoke the authenticated device.
- `POST /v1/device/identities`: register `{identity, public_key, role?}` in the token's tenant.
- `DELETE /v1/device/identities/<id>`: revoke an owned identity.
- `POST /v1/credentials/nats`: device bearer token plus `{identity, public_key}`;
  no endpoint service key. Tenant, device, revocation and registered key are checked.
- `POST /device/service-key`: DEV admin bootstrap with `X-ACS-Service-Key`;
  returns a new device and its one-time displayed secret. `DELETE` with
  `{device}` and the same admin header revokes that bot device and key.

Device access-token audience is the control-plane issuer. These tokens cannot
be used as remote MCP tokens. Secrets are persisted hashed in the stub; refresh
replay revokes the device. Identity names obey `check_identity`, are unique
within a tenant (including revoked names), and roles are worker/orchestrator.

```bash
make setup && scripts/dev/install_nsc.sh && make build
scripts/dev/operator_stack.sh .venv/bin/python -m pytest -q -s tests/python/test_acs_e2e.py
```

This CI test starts the stub, uses real RFC 8628 verification, registers `me`,
installs Claude config, launches exactly that stdio MCP configuration, and gets
an echo result. It runs `acs up --foreground` with 60-second NATS credentials,
proves the same Rust HubClient and Python MCP/worker survive expiry, rejects
an unrefreshed copy, and confirms identity removal prevents renewal. The
unit/CLI tests also cover service keys, device/tenant boundaries, refresh
rotation/replay and preservation of all three clients' unrelated settings.
