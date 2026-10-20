# Remote install — join a hub from your laptop

This is the operator **and** user-facing runbook for getting a remote
machine (usually a teammate's laptop) onto a nats-hub **without cloning
the whole monorepo**. If you just want the 30-second join, jump to
[Quick start](#quick-start). If you are the operator deciding what to
ship to whom, read [When to ship the thin package](#when-to-ship-the-thin-package-vs-full-clone)
first.

> **Already read [`JOIN_HUB.md`](JOIN_HUB.md)?** That page covers the
> adapter's flags and the two auth modes (token vs credentials file) in
> depth. This page is about **packaging** — what files to put on the
> remote machine and how to install them. The two complement each other:
> JOIN_HUB explains the *what* and *why*; REMOTE_INSTALL explains the
> *how to ship it*.

---

## When to ship the thin package vs full clone

| Situation | Ship |
|---|---|
| Teammate just wants to run a worker (shell, kilo, opencode) against the hub | **Thin package** (this doc) |
| Teammate is air-gapped or has no git access to the repo | **Thin package**, copied by hand |
| Teammate will edit adapter / runtime code, file PRs, run the Rust binaries | **Full clone** + dev toolchain |
| You are running the hub itself | [`OPERATOR_HUB.md`](OPERATOR_HUB.md) |

The thin package is roughly **9 files** plus a venv with one pip package
(`nats-py`). It contains zero Rust, zero SurrealDB, zero daemon. The
adapter speaks NATS-over-WebSocket to the hub; everything else stays on
the hub host.

---

## Quick start

On the remote machine (Python 3.10+ required):

```bash
# Option A: from a GitHub release (no clone). The bundle is downloaded over
# HTTPS and checked against the release's SHA256SUMS before anything is copied.
curl -fsSL -o install.sh \
  https://raw.githubusercontent.com/jarmen423/agent-communication-server/main/packaging/remote/install.sh
bash install.sh --from-release latest          # or a tag, e.g. --from-release v0.2.0
# → installs into ~/nats-hub-remote

# Option B: from a clone of the repo (uses the checkout's files)
git clone https://github.com/jarmen423/agent-communication-server.git nats-hub && cd nats-hub
bash packaging/remote/install.sh

# Option C: air-gapped. Either copy the release's nats-hub-remote-<tag>.tar.gz
# and SHA256SUMS into a directory and run
#   bash install.sh --from-release <tag> --release-base-url file:///that/dir --no-pip
# or copy packaging/remote/ + the files from FILES.txt into a flat directory, then:
bash install.sh
```

A checksum mismatch aborts the install. If the release can't be reached,
`--from-release` falls back to the local checkout when there is one.

Then activate and launch:

```bash
source ~/nats-hub-remote/.venv/bin/activate
cd ~/nats-hub-remote

export NATS_TOKEN='<token-from-operator>'
python3 remote_agent_adapter.py \
    --identity my-worker-1 \
    --nats-url wss://hub.example.com:8080 \
    --ca-file ~/hub-ca.crt \
    --backend shell \
    --execute "my-agent-cli --prompt"
```

If you see `[remote-adapter] identity=my-worker-1 backend=shell
nats_url=wss://…`, you are on the bus. Delegate a test task from the hub
and watch it round-trip.

---

## What's in the thin package

| Path | Why |
|---|---|
| `remote_agent_adapter.py` | Entry point. |
| `nats_connect.py` | Shared connect helper (auth + TLS). |
| `worker_runtime.py` | The worker loop (subscribe → run backend → publish). |
| `worker_events.py` | Structured progress events. |
| `worker_backends/__init__.py` | Backend package init. |
| `worker_backends/headless_cli.py` | `HeadlessCliBackend` + spec dataclass. |
| `worker_backends/proc.py` | Subprocess plumbing (process groups, timeouts) imported by `headless_cli.py`. |
| `worker_backends/presets.py` | `kilo_spec()`, `opencode_spec()`, `hermes_spec()`, `agy_spec()`, `grok_spec()`. |
| `worker_backends/sdk_agent.py` | In-process SDK backend (re-exported by `__init__.py`). |
| `requirements-remote.txt` | One line: `nats-py>=2.6.0`. |

Authoritative list: [`packaging/remote/FILES.txt`](../packaging/remote/FILES.txt).
The list is verified by importing `remote_agent_adapter` in a clean
interpreter and recording every module pulled in; the closure contains
no third-party packages other than `nats-py`.

### Not included

- The Rust binaries (`hub-server`, `hub-publish`, …). Remote workers
  don't need them — they speak NATS, not the Rust CLI.
- SurrealDB. Persistence lives on the hub host.
- The ACP-over-HTTP backends (`acp_http.py`, `kilo_acp.py`,
  `hermes_acp.py`, `opencode_acp.py`, `grok_acp.py`). They need `httpx`
  and aren't imported by the adapter at module top level. If you start
  using one, add the file to `FILES.txt` (which `install.sh` and the release
  bundle both read) and `httpx>=0.27` to `requirements-remote.txt`.

---

## Worked example 1 — `shell` backend

The shell backend runs any CLI: the prompt is appended as the last
argument. Use this to wrap any in-house agent that has a `--prompt`
style flag.

```bash
source ~/nats-hub-remote/.venv/bin/activate
cd ~/nats-hub-remote

export NATS_TOKEN='<token-from-operator>'

python3 remote_agent_adapter.py \
    --identity laptop-shell-1 \
    --nats-url wss://hub.example.com:8080 \
    --ca-file ~/hub-ca.crt \
    --backend shell \
    --execute "my-agent-cli --prompt" \
    --repo ~/code/myproject
```

What happens when a task arrives:

1. Router delivers the prompt to `channel.inbox.laptop-shell-1`.
2. Adapter runs `my-agent-cli --prompt "<the task>"` in `~/code/myproject`.
3. Adapter publishes the stdout back to the task channel.
4. Sender (or session) sees a `completed` event with the result.

`--repo` only matters if your CLI cares about its `cwd` (most do).

---

## Worked example 2 — `kilo` backend

The kilo backend shells out to `kilo run --format json --auto`, parsing
NDJSON for structured events. Each hub session maps to a kilo session
(resume is automatic on follow-up turns).

```bash
source ~/nats-hub-remote/.venv/bin/activate
cd ~/nats-hub-remote

python3 remote_agent_adapter.py \
    --identity laptop-kilo-1 \
    --nats-url wss://hub.example.com:8080 \
    --ca-file ~/hub-ca.crt \
    --backend kilo \
    --model anthropic/claude-sonnet-4.5 \
    --repo ~/code/myproject
```

Notes:

- `kilo` must be on `PATH` (or pass `--kilo-bin /path/to/kilo`).
- `--repo` becomes kilo's working directory; the agent sees your checkout.
- Auth here uses a credentials file (NKEY/JWT) instead of a token:

  ```bash
  python3 remote_agent_adapter.py \
      --identity laptop-kilo-1 \
      --nats-url wss://hub.example.com:8080 \
      --credentials-file ~/.nats/laptop-kilo-1.creds \
      --backend kilo \
      --model anthropic/claude-sonnet-4.5
  ```

`opencode` is identical except `--backend opencode` and `--model` is
optional. See [`JOIN_HUB.md`](JOIN_HUB.md#other-backends) for the full
backend table.

---

## Authentication cheat sheet

All flags are optional and have `NATS_*` env-var fallbacks (CLI flag
wins). See [`REMOTE_AGENTS.md` §Authentication](REMOTE_AGENTS.md#authentication)
for the full matrix.

| You have | Flag(s) | Env var(s) |
|---|---|---|
| Shared token | `--token <t>` | `NATS_TOKEN` |
| User + password | `--user <u> --password <p>` | `NATS_USER`, `NATS_PASSWORD` |
| `.creds` file (NKEY/JWT) | `--credentials-file <path>` | `NATS_CREDENTIALS_FILE` |
| `.nk` seed file | `--nkeys-seed <path>` | `NATS_NKEYS_SEED` |
| mTLS client cert | `--cert-file <c> --key-file <k>` | `NATS_CERT_FILE`, `NATS_KEY_FILE` |
| Private-CA server cert | `--ca-file <ca>` | `NATS_CA_FILE` |

TLS is enabled automatically when the URL is `wss://` or `tls://`, or
when any cert/key flag is given. With no `--ca-file`, the system trust
store is used so publicly trusted certs (Let's Encrypt etc.) work with
zero extra flags.

---

## Security checklist

These rules apply whether you are the operator handing out credentials
or the teammate receiving them.

1. **Never bake a token into a script.** `install.sh` deliberately
   refuses to accept credentials. The credential is supplied at adapter
   launch time, via env var or CLI flag. Don't edit the script to embed
   one — you'll forget it's there and the next person will inherit it.

2. **Store secrets in env vars or a password manager.** A token in
   `~/.bashrc` is acceptable on a single-user laptop. A token in a
   committed `.env` is not. A token in a script under version control
   is a bug.

3. **`.creds` / `.nk` files are SSH-key-grade secrets.** Store at
   `chmod 0600`, never commit, rotate if the laptop is lost or shared.

4. **Prefer per-agent credentials over a shared token.** With the
   `users` auth mode the operator can revoke one agent's credential
   without rotating everyone's. See [`SECURITY.md`](SECURITY.md)
   Layer 2 (Authentication).

5. **Confirm the subject allowlist with the operator.** A remote worker
   only needs to publish `hub.send.<channel>`, `hub.register`,
   `hub.presence`, `hub.events.>` and subscribe to
   `channel.inbox.<identity>` (plus any task/session channels it joins).
   Anything broader is a sign the operator's allowlist is too loose —
   flag it. See [`SECURITY.md`](SECURITY.md) Layer 3 (Authorization).

6. **Verify the server cert, don't bypass it.** `--tls-insecure` is
   fail-closed: it only takes effect if the operator also sets
   `NATS_ALLOW_INSECURE=1`. Do not set that env var in production. If
   the hub uses a self-signed cert, ship the CA file and use `--ca-file`.

7. **If you suspect a credential leaked, tell the operator immediately.**
   Rotation is cheap; pretending it didn't happen is expensive.

Full model: [`SECURITY.md`](SECURITY.md).

---

## Updating a remote install

When the adapter, runtime, or backends change upstream, re-run the
installer from a newer release or a current checkout:

```bash
bash install.sh --from-release latest     # refreshes ~/nats-hub-remote
# or
cd nats-hub && git pull
bash packaging/remote/install.sh
```

`install.sh` overwrites the copied files and re-runs pip. Your venv and
target dir are preserved; no credential is touched.

If a new file joined the import closure (you'll see an `ImportError` on
launch), add it to [`packaging/remote/FILES.txt`](../packaging/remote/FILES.txt)
and re-run. `install.sh` reads that list; its built-in list is only a
fallback for hand-copied layouts without `FILES.txt`.

---

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `ModuleNotFoundError: nats` | Venv not activated. `source ~/nats-hub-remote/.venv/bin/activate`. |
| `ImportError: worker_runtime` | A file is missing from the target dir. Re-run `install.sh` from a current checkout. |
| `ConnectionRefusedError` | Wrong URL/port, or firewall blocks the WS port (default 8080). |
| `ssl: certificate_verify_failed` | Mismatched `--ca-file`, or URL hostname doesn't match cert SAN. |
| `Authorization Violation` | Wrong token, wrong user/password, or your user isn't on the `WEBSOCKET` connection type. |
| Adapter connects but you never receive tasks | Your subject allowlist doesn't include `channel.inbox.<identity>`. Ask the operator. |
| `--tls-insecure` "refused" error | Operator hasn't set `NATS_ALLOW_INSECURE=1`. That's intentional — don't work around it; ship the CA file. |

More in [`JOIN_HUB.md` §Troubleshooting](JOIN_HUB.md#troubleshooting).

---

## See also

- [`JOIN_HUB.md`](JOIN_HUB.md) — the end-user join runbook (auth modes,
  verifying your connection, troubleshooting).
- [`REMOTE_AGENTS.md`](REMOTE_AGENTS.md) — architecture, the full
  backend table, and the authentication flag/env matrix.
- [`SECURITY.md`](SECURITY.md) — the four-layer security model.
- [`OPERATOR_HUB.md`](OPERATOR_HUB.md) — standing up the hub itself.
- [`packaging/remote/README.md`](../packaging/remote/README.md) — the
  bundle the operator ships to the teammate.
