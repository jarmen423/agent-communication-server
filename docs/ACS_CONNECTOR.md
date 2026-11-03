# ACS device connector

`acs` is the Apache-2.0 Rust connector in `crates/acs`, built on
`nats-hub-client`. It manages a device's credentials; agents connect directly
to the tenant's NATS account. By default it signs in to hosted ACS at
`https://acs.agentmemorylabs.com`. The stub control plane in
`stub_control_plane/` is **DEV ONLY**: its verification auto-approves the
fixed dev user. Do not expose it with real data.

## Install and sign in

Release tarballs ship `acs` beside `hub-*`. The README's hosted quick start
has the install line: `scripts/install_remote.sh` puts the binaries in
`~/.local/share/nats-hub/bin/` and links them into `~/.local/bin`. From
source run `make setup && make build`, or use the remote installer with
`--from-source --with-acs`. The connector itself links no server or database
code.

```bash
acs login                                # hosted ACS; opens the browser
acs login --headless                     # or print a URL to open on another device
acs agent add me                         # orchestrator by default
acs agent add laptop-worker --role worker
acs up                                   # install/start a user service
acs status                               # identities and credential expiry
acs worker run claude --identity laptop-worker -- --repo ~/code/project
acs mcp install claude --identity me     # an MCP orchestrator, or plain CLIs:
acs exec me -- hub-delegate --to laptop-worker --prompt "What is 2+2?"
```

`--server` (or `ACS_SERVER`) picks another control plane: a self-hosted one,
or the DEV ONLY stub with `--server http://127.0.0.1:9300`. Only HTTPS
control-plane URLs are accepted, except HTTP on loopback for the stub. Login
returns the tenant's NATS URL and keeps it in the device state, so nobody
types it. The stub's `--nats-url` sets the URL it returns; pass the operator
stack's private `NATS_URL` when running a local demo. HTTP redirects aren't
followed with bearer credentials.

`acs login --key KEY` exchanges a long-lived, revocable **per-device** secret.
Prefer `ACS_SERVICE_KEY` to avoid shell-history exposure. The key is exchanged
for the usual short-lived access token and rotating refresh token; the CLI
does not retain the service key. No NATS user seed is sent to the issuer.

## Run CLIs as an identity

The `hub-*` CLIs and your own scripts need three variables: `NATS_URL`,
`NATS_CREDENTIALS_FILE` and `NATS_HUB_IDENTITY`. `acs` supplies them from the
device state:

```bash
acs exec me -- hub-agents                # one command
eval "$(acs env me)"                     # or the current shell
hub-history --tail
```

`acs exec <id> -- <cmd> [args…]` replaces itself with `cmd`, so signals and
the exit status are the command's own. A bare command name is looked up on
PATH, then next to `acs`. Like `acs worker run`, it removes the other NATS
auth variables (`NATS_USER`, `NATS_PASSWORD`, `NATS_TOKEN`, `NATS_NKEY`,
`NATS_NKEYS_SEED`) and `ACS_SERVICE_KEY` from the command's environment: a
token left over from a self-hosted setup would otherwise be sent along with
the identity's credentials. `acs env <id>` prints an `unset` line for the same
NATS variables, then the three `export` lines, POSIX-quoted. Both warn on
stderr when the credential has expired or hasn't been issued yet; `acs up`
starts the daemon that renews it.

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

`acs status` lists each identity with its role and credential expiry, as
RFC 3339 UTC plus a relative time: `me: orchestrator credential expires
2026-11-04T21:18:56Z (in 23h)`, or `expired … (10m ago)`.

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

## MCP clients

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

The installer never adopts code from the current directory: the interpreter
and script it finds are written into the agent's config and run on every
session with the identity's credentials. It looks next to the `acs` binary
and in `~/.local/share/nats-hub/remote` (the worker bundle). To use a checkout,
say so explicitly with `ACS_MCP_SERVER`/`--server` (and optionally
`ACS_PYTHON`/`--python`). A standalone binary embeds the canonical Python modules,
writes them under `ACS_HOME/mcp/server`, and creates a private venv with pinned
nats-py and the MCP dependencies. This first install needs Python 3.10+ with
venv/pip and network access. It does not embed a Python interpreter.

## Workers

`acs worker run <backend> --identity <id> -- <worker args>` starts a worker as
that identity:

```bash
acs worker run claude --identity laptop-worker -- --repo ~/code/project
acs worker run codex --identity codex-worker -- --repo ~/code/project
acs worker run echo --identity echo-1                # no LLM; for smoke tests
acs worker run hub-worker --identity cat-1 -- --execute cat
```

The Python workers are built into `acs`. The build embeds every
`*_worker.py` entrypoint except `cursor`, plus every repository module they
import (`worker_runtime.py`, `worker_events.py`, `nats_connect.py`,
`hub_delivery.py` and the whole `worker_backends` package); see
`crates/acs/build.rs`. That covers `echo`, `claude`, `codex`, `hermes`,
`hermes_acp`, `grok`, `grok_acp`, `kilo`, `kilo_acp`, `opencode`,
`opencode_acp` and `agy`. The first `acs worker run` writes the modules to
`ACS_HOME/worker/<version>-<hash>/` and creates a private venv in
`ACS_HOME/worker/.venv` with pinned nats-py, nkeys, aiohttp and httpx. That
first run needs Python 3.10+ with venv/pip and network access; later runs
start at once. The directory name follows the embedded code, so an upgraded
`acs` never mixes old and new modules. Once no worker from an old version is
running, its directory can be deleted. Files that differ from the binary's
copy are rewritten on every run. The agent CLI itself (`claude`, `codex`, …)
still has to be installed and signed in.

The code comes from, in order:

1. `ACS_WORKER_DIR`: a trusted checkout you name explicitly (with its `.venv`
   when present);
2. the runtime built into `acs`;
3. the worker bundle in `~/.local/share/nats-hub/remote`, for a backend `acs`
   doesn't embed.

`ACS_PYTHON` overrides the interpreter in every case. `cursor` needs
`cursor-sdk`, so it runs only from a checkout, with an `ACS_PYTHON` that has
the SDK. The current directory is never searched: the worker gets the
identity's credentials, so running `acs worker run` inside an untrusted
checkout must not run its code.

`hub-worker` uses the sibling binary or PATH and accepts its usual extra
flags. Every backend gets `--identity` and `--nats-url` (the tenant URL) ahead
of your extra args, the same three environment variables as MCP, and the same
auth stripping as `acs exec`. `acs` then execs the worker, so signals and the
exit status pass straight through.

**Hosted users don't need the worker bundle.** `packaging/remote/install.sh`
is for self-hosted hubs, where an operator issues the credential. It installs
into `~/.local/share/nats-hub/remote`, the directory `acs` checks.

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
- `POST /device/service-key`: DEV admin bootstrap with `X-ACS-Admin-Key` (from `--admin-key-file`, default `<service-key-file>.admin`; deliberately not the MCP endpoint's service key);
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
results from an echo worker and a claude worker (on the fake CLI). Both run
from the runtime built into `acs`, as from a release install: no
`ACS_WORKER_DIR`, no `ACS_PYTHON` (so `acs` builds its private venv) and a
working directory outside the checkout. It runs `acs up --foreground` with 60-second NATS credentials,
proves the same Rust HubClient and Python MCP/worker survive expiry, rejects
an unrefreshed copy, and confirms identity removal prevents renewal. The
unit/CLI tests also cover service keys, device/tenant boundaries, refresh
rotation/replay and preservation of all three clients' unrelated settings.

## Known limits (security review, 2026-10-21)

- **One OS user = one trust domain.** Every identity's seed (`creds/*.creds`)
  and, without a keychain, the device refresh token live under `ACS_HOME`,
  readable by any process of that user. An LLM worker with shell access can
  read the orchestrator's credentials. Run untrusted workers as a separate
  OS user or in a container that only gets its own `.creds`.
- **Revocation is not instant for issued credentials.** `acs agent rm` and
  `acs logout` stop renewal; NATS JWTs already issued stay valid until they
  expire (stub default 1 h). The production control plane should push
  account revocations.
- **Refresh rotation is strict.** A refresh token replayed after a lost
  response revokes the device (no grace window yet).
- **`acs login --key KEY`** puts the key on the command line (visible in
  `/proc`); prefer `ACS_SERVICE_KEY`.
- **Config files.** Merging rewrites `~/.claude.json` / `~/.codex/config.toml`
  / `~/.hermes/config.yaml` through a temp file and rename: a symlinked
  config becomes a regular file, YAML comments are dropped, and a concurrent
  write by the agent itself can be lost.
- **The stub's device flow is DEV ONLY**: `/device/verify` approves on GET,
  user codes are short and unthrottled, and identity names are never
  reusable. Production needs an authenticated, CSRF-protected approval page,
  rate limits and quotas.
