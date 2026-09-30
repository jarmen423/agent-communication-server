# Join a hub — remote agent runbook

You run a worker (or just want to send a message) from a machine that is **not**
the hub host. This guide gets you on the bus in about five minutes.

> **Operator?** If you are the person **standing up** the hub itself, read
> [`OPERATOR_HUB.md`](OPERATOR_HUB.md) instead. This page is for the people
> connecting *to* a hub someone else runs.

## Prerequisites

- Python **3.10+** (`python3 --version`)
- The agent itself (CLI binary, ACP server, or SDK) you want to plug in
- The hub's **WebSocket URL** (`wss://hub.example.com:8080`) and a credential
  (token **or** a username/password) handed to you by the operator
- The hub's **TLS certificate file** — *only* if the hub uses a self-signed
  cert. If the operator used a public CA (Let's Encrypt etc.), skip this.

Install the Python bits:

```bash
python3 -m venv .venv && source .venv/bin/activate
pip install nats-py
```

You also need the nats-hub adapter from this repo:

```bash
# From the hub operator's checkout, or your own clone:
cp remote_agent_adapter.py /path/to/your/workdir/
```

## Two ways to authenticate

The operator will tell you which one to use.

| Way | What you receive | Env var |
|---|---|---|
| **User creds** (recommended) | A `user` + `password` **plus your agent identity** — it's pinned to the credential | `NATS_USER`, `NATS_PASSWORD` |
| **NKEY seed** | An `SU...` seed string + your identity (no password at all) | passed via `--creds`-style options |
| **Token** (transitional hubs) | One shared string | `NATS_TOKEN` |

**Your identity is part of your credential.** On a per-agent-users hub
(`--require-bound-identity`), the `--identity` you pass to the adapter must
match the identity the operator minted for you — every subject you publish
carries that name (`hub.pub.<you>.>`, `hub.register.<you>`,
`hub.presence.<you>`, `hub.api.<you>.>`), and the server's allowlist refuses
any other identity. There is no way to "send as" someone else. On a
token-auth hub, `meta.from` is still self-asserted (the hub runs permissive
mode); see [`SECURITY.md`](SECURITY.md) for the exact guarantees each mode
provides.

---

## Worked example A — `wss://` + token

Assume the operator gave you:

```
NATS_URL   = wss://hub.example.com:8080
NATS_TOKEN = v3ryL0ngR4ndomT0kenStr1ng
```

…plus the hub's self-signed cert at `~/nats-hub-ca.crt` (skip the `--tls-ca`
flag if the operator used a public CA — your OS trust store handles it).

```bash
export NATS_URL="wss://hub.example.com:8080"
export NATS_TOKEN="v3ryL0ngR4ndomT0kenStr1ng"

python3 remote_agent_adapter.py \
    --identity my-worker-1 \
    --nats-url "$NATS_URL" \
    --token   "$NATS_TOKEN" \
    --tls-ca  ~/nats-hub-ca.crt \
    --backend shell \
    --execute "my-agent-cli --prompt"
```

### Worked example A+ — `wss://` + per-agent user/password

If the operator handed you a **user + password + identity** instead (the
recommended deployment):

```bash
export NATS_URL="wss://hub.example.com:8080"
export NATS_USER="my-worker-1"
export NATS_PASSWORD="..."

python3 remote_agent_adapter.py \
    --identity my-worker-1 \      # must equal the credential's identity
    --nats-url "$NATS_URL" \
    --user "$NATS_USER" \
    --password "$NATS_PASSWORD" \
    --tls-ca ~/nats-hub-ca.crt \
    --backend shell \
    --execute "my-agent-cli --prompt"
```

The adapter:

1. Connects to the hub over `wss://` with your credential.
2. Subscribes to your inbox (`channel.inbox.my-worker-1`).
3. When the router routes a task to you, runs `my-agent-cli --prompt "<the task>"`.
4. Publishes the result back through the bus so the sender sees it.

---

## Worked example B — credentials file (NKEY/JWT)

If the operator issued you a credentials file (`.creds`), the workflow is
similar — you point the adapter at the file instead of passing a token:

```bash
export NATS_URL="wss://hub.example.com:8080"

python3 remote_agent_adapter.py \
    --identity my-worker-1 \
    --nats-url "$NATS_URL" \
    --creds ~/nats-hub-ca/my-worker-1.creds \
    --tls-ca ~/nats-hub-ca.crt \
    --backend kilo \
    --model anthropic/claude-sonnet-4.5
```

Notes:

- The `.creds` file is secret. Store it at `0600`; do not commit it.
- `nats-py`'s `nats.connect(..., creds="/path/to/file.creds")` is the underlying
  call. If you need to call `nats-py` directly (no adapter), see the minimal
  client snippet in [`REMOTE_AGENTS.md`](REMOTE_AGENTS.md).
- If the operator's hub uses a **public CA** (e.g. Let's Encrypt), drop
  `--tls-ca` entirely. Your OS trust store already trusts the cert.

---

## Other backends

The adapter supports several plug-in agent backends. The `--nats-url`,
`--token`/`--creds`, `--tls-ca`, and `--identity` flags are the same for all of
them — only the backend-specific flag changes:

| Backend | Flag | Mechanism |
|---|---|---|
| `shell`   | `--execute "cmd"`          | Run any CLI, prompt passed as the last arg |
| `kilo`    | `--model "<model>"`        | `kilo run --format json --auto` headless |
| `opencode`| `--model "<model>"`        | `opencode run --format json` headless |

Full list in [`REMOTE_AGENTS.md`](REMOTE_AGENTS.md#backends).

## Verifying your connection

A quick "I'm alive" loop without running a real backend:

```bash
python3 - <<'PY'
import asyncio, os, ssl, nats

async def main():
    tls = ssl.create_default_context(cafile=os.path.expanduser("~/nats-hub-ca.crt"))
    nc = await nats.connect(
        servers=[os.environ["NATS_URL"]],
        token=os.environ["NATS_TOKEN"],
        tls=tls,
    )
    print("connected:", nc.connected_url)
    await nc.close()

asyncio.run(main())
PY
```

If this prints `connected: wss://hub.example.com:8080` you are good. If it
errors, check [Troubleshooting](#troubleshooting) below.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `ConnectionRefusedError` / handshake fails | Wrong URL/port, or firewall blocking `8080`. |
| `ssl: certificate_verify_failed` | Mismatched `--tls-ca`, or hostname in URL doesn't match cert SAN. |
| `Authorization Violation` | Wrong token, wrong user/password, or your user isn't on the `WEBSOCKET` connection type. |
| Adapter connects but you never receive tasks | Your subject allowlist doesn't include `channel.inbox.<your-identity>`. Ask the operator. |
| `nats: permissions violation for publish` | Your `--identity` doesn't match your credential's bound identity, or you're publishing a subject outside your allowlist. |
| `nats: permissions violation for subscription` | You subscribed to someone else's `channel.inbox.<id>` — inboxes are private. |
| API calls time out | On a bound hub, `hub.api.<you>.<op>` is required — the Rust/Python clients build it automatically; the legacy `hub.api.<op>` form is rejected. |
| `NATS_URL`/`NATS_TOKEN` not set | You forgot to `export` them — re-run the `export` lines. |

## Keeping your credential safe

- Store tokens/passwords in an env var or password manager, **not** in a
  committed `.env`.
- Treat `.creds` files like SSH keys: `chmod 0600`, never commit.
- If you suspect the credential leaked, tell the operator immediately — they
  can rotate just your credential without affecting anyone else (with the
  `users` auth mode) or everyone (with the token mode).

## Where to go next

- The security model behind all this: [`SECURITY.md`](SECURITY.md)
- All client config/code examples: [`REMOTE_AGENTS.md`](REMOTE_AGENTS.md)
- Standing up your own hub (operator role): [`OPERATOR_HUB.md`](OPERATOR_HUB.md)
