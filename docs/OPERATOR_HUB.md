# Operator runbook — stand up a hub

This is the operator-side guide to going from a fresh VPS to a working,
hardened nats-hub that your team can dial into. Read it end-to-end once, then
use the [verification checklist](#verification-checklist) for every new hub.

> **End user (joining a hub someone else runs)?** Read
> [`JOIN_HUB.md`](JOIN_HUB.md) instead. **Plain-language security model?**
> [`SECURITY.md`](SECURITY.md).

## What you are building

A single host running three things:

```
┌──────────────────────────────────────────────────────────┐
│  Hub host  (one VPS)                                     │
│                                                          │
│   ┌──────────────────┐   ┌────────────────────┐          │
│   │ nats-server      │   │ hub-server          │          │
│   │ :4222 (loopback) │◀──│ (router + DB)       │          │
│   │ :8080 (wss://)   │   │ RocksDB /var/lib    │          │
│   │ :8222 (loopback) │   └────────────────────┘          │
│   └────────▲─────────┘                                    │
│            │                                             │
└────────────┼─────────────────────────────────────────────┘
             │ wss://hub.example.com:8080
             │
   ┌─────────┴────────┬─────────────────┐
   ▼                  ▼                 ▼
 team machine A   team machine B    team machine C
 (remote_agent_   (remote_agent_    (remote_agent_
  adapter.py)      adapter.py)       adapter.py)
```

- **nats-server** is the bus. It listens on `4222` (loopback, for hub-server)
  and `8080` (the WebSocket gateway remote agents connect to).
- **hub-server** is the nats-hub router. It connects to the bus on loopback
  and mirrors every envelope into an embedded RocksDB.
- Each team machine runs `remote_agent_adapter.py` and dials in over `wss://`.

## 0. Pick the host

A 2 vCPU / 4 GB RAM VPS comfortably handles a small team (dozens of agents,
low-thousands of messages/sec). Bump RAM if you keep a long RocksDB history.

You need:

- A public DNS name (e.g. `hub.example.com`) pointing at the VPS.
- Root or sudo access.
- Ports `8080/tcp` reachable from wherever your team connects (or only on the
  VPN interface if everyone is on a VPN).

## 1. Install binaries

```bash
# NATS server binary
curl -L https://github.com/nats-io/nats-server/releases/latest/download/nats-server-v2.10.0-linux-amd64.tar.gz \
    | tar -xz -C /tmp
sudo install -m 0755 /tmp/nats-server-v2.10.0-linux-amd64/nats-server /usr/local/bin/nats-server

# hub-server binary — build from this repo and copy in
cargo build --release --bin hub-server
sudo install -m 0755 target/release/hub-server /usr/local/bin/hub-server
```

Verify:

```bash
nats-server   --version
hub-server    --help | head -3
```

## 2. Create system users and directories

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin nats
sudo useradd --system --no-create-home --shell /usr/sbin/nologin \
    --home-dir /var/lib/nats-hub nats-hub

sudo install -d -m 0750 -o nats -g nats     /etc/nats /etc/nats/tls
sudo install -d -m 0750 -o nats -g nats     /var/log/nats-hub
sudo install -d -m 0750 -o nats-hub -g nats-hub /var/lib/nats-hub
```

## 3. TLS certificate

You need a cert whose **Subject Alt Name includes the DNS name clients will
use**. Two paths:

### 3a. Public CA (recommended for internet-facing hubs)

Use any ACME client. Caddy / certbot / lego all work. Example with `certbot`
in standalone mode (open port 80 first):

```bash
sudo certbot certonly --standalone -d hub.example.com
sudo install -m 0644 -o nats -g nats /etc/letsencrypt/live/hub.example.com/fullchain.pem /etc/nats/tls/nats-server.crt
sudo install -m 0600 -o nats -g nats /etc/letsencrypt/live/hub.example.com/privkey.pem   /etc/nats/tls/nats-server.key
```

Clients then use their OS trust store — no CA file to ship. Set up renewal
(`certbot renew` in cron) and re-copy the files after renewal.

### 3b. Self-signed (OK for internal / VPN-only hubs)

```bash
sudo install -d -m 0750 -o nats -g nats /etc/nats/tls
sudo openssl req -x509 -newkey rsa:4096 -sha256 -nodes \
    -days 365 \
    -keyout /etc/nats/tls/nats-server.key \
    -out    /etc/nats/tls/nats-server.crt \
    -subj "/CN=hub.example.com" \
    -addext "subjectAltName=DNS:hub.example.com"
sudo chown nats:nats /etc/nats/tls/nats-server.key /etc/nats/tls/nats-server.crt
sudo chmod 0600 /etc/nats/tls/nats-server.key
sudo chmod 0644 /etc/nats/tls/nats-server.crt
```

You must ship `/etc/nats/tls/nats-server.crt` to every client (it is its own
CA in this case). Point clients at it with `--tls-ca`.

## 4. Configure NATS

Copy the template and edit it in place:

```bash
sudo install -m 0640 -o nats -g nats \
    config/nats-server.prod.conf.example /etc/nats/nats-server.conf
sudo $EDITOR /etc/nats/nats-server.conf
```

Pick **exactly one** of the two authN modes:

- **`token`** — one shared secret. Transitional only: it authenticates the
  connection but binds no identity, so `meta.from` stays self-asserted and
  every inbox is readable by every credential holder. Run hub-server WITHOUT
  `--require-bound-identity` in this mode.
- **`users`** — per-agent credentials **(required for identity binding)**.
  Each agent's allowlist pins every send/register/heartbeat/API subject to
  its own identity (contract §4.1); `allowed_connection_types:["WEBSOCKET"]`
  keeps agent credentials off the loopback TCP listener. See
  [`SECURITY.md`](SECURITY.md) Layer 3.

**Do not hand-write the users[] entries** — generate them:

```bash
# On any machine with the repo built (cargo build --bin hub-admin)
hub-admin add-agent echo-1 --role worker --channel chat        # prints one entry
hub-admin add-agent boss --role admin --nkey                   # nkey instead of password
```

The prod template (`config/nats-server.prod.conf.example`) shows the full
shape including the `hub-server` service user. For a fleet, keep an
`agents.txt` (lines: `<id> <role> <pw:pass|nkey:PUB> [ws-only] [channels]`)
and regenerate the whole file:

```bash
hub-admin render-config --agents agents.txt \
    --tls-cert /etc/nats/tls/nats-server.crt \
    --tls-key  /etc/nats/tls/nats-server.key \
    --hub-password "$HUB_SERVER_PW" \
    --out /etc/nats/nats-server.conf
```

`render-config` always emits a `hub-server` service user; pass its password
back to hub-server via `NATS_USER=hub-server NATS_PASSWORD=...` (an
`EnvironmentFile` on the systemd unit is the usual place).

Request-reply is per identity: each user may subscribe only its own reply
inbox `_INBOX.<id>.>` and none may publish to `_INBOX` at all. Processes that
**answer** requests need a responder role: run the worker supervisor as a
`--role service` user (it gets `allow_responses: true`); the generated
`hub-server` user already has it. Only `orchestrator`/`admin`/`service` roles
may send worker-supervisor requests (`hub.worker.*`). `hub-admin` refuses the
reserved identities `hub-server`, `hub-orchestrator` and `visualizer`,
duplicate identities, and `--channel`s with wildcards or inbox overlap.

**Run hub-server in bound mode** so the hub layer enforces what the ACLs
claim (see SECURITY.md § threat model):

```bash
hub-server --db-path /var/lib/nats-hub/nats_hub.db \
    --require-bound-identity \
    --api-admin boss          # repeatable; or NATS_HUB_API_ADMINS=a,b
```

**Generating strong secrets** (hub-admin generates passwords/nkeys for you;
for anything by hand):

```bash
openssl rand -hex 32      # token
openssl rand -base64 24   # password
```

Lock the config down:

```bash
sudo chmod 0600 /etc/nats/nats-server.conf
sudo chown nats:nats /etc/nats/nats-server.conf
```

## 5. Firewall

The minimum exposure rule:

```bash
# 8080 — the WebSocket gateway. Open to the world, or just to the VPN.
sudo ufw allow 8080/tcp

# 4222 — NEVER open to the world. VPN/LAN only:
# sudo ufw allow in on wg0 to any port 4222 proto tcp   # wireguard example

# 8222 — loopback only. The nats-server config already binds it to 127.0.0.1.

# 22 — your SSH port, naturally.
sudo ufw allow OpenSSH
sudo ufw enable
sudo ufw status verbose
```

If you are **not** using ufw, the equivalent iptables / cloud-provider security
group is: allow `8080/tcp` from your team's source IPs; allow `4222/tcp` only
from loopback / VPN; allow `8222/tcp` from loopback only.

## 6. Install systemd units

```bash
sudo install -m 0644 deploy/systemd/nats-hub-nats.service   /etc/systemd/system/
sudo install -m 0644 deploy/systemd/nats-hub-server.service /etc/systemd/system/
sudo systemctl daemon-reload
```

## 7. Validate BEFORE starting

```bash
nats-server -t -c /etc/nats/nats-server.conf
# Expect: ... "OK" ... / "configuration file ... is valid"

systemd-analyze verify /etc/systemd/system/nats-hub-*.service
# Expect: no output (= success). Any output is a unit-file error to fix.
```

Do not skip this step. A typo in the NATS config will hot-loop the unit under
`Restart=on-failure`.

## 8. Start services

```bash
sudo systemctl enable --now nats-hub-nats.service
sleep 2
sudo systemctl enable --now nats-hub-server.service
```

## 8b. Durable delivery (optional, recommended for remote/laptop agents)

To keep DMs for offline agents, survive hub-server restarts and let
delegators resume results (details: [`DELIVERY.md`](DELIVERY.md)):

1. Render the config with `hub-admin render-config ... --durable
   --js-store-dir /var/lib/nats-hub/jetstream` (or uncomment the
   `jetstream {}` block in the prod example). Create the directory, owned
   by the NATS service user.
2. Add `--durable` to hub-server's `ExecStart` (optionally
   `--delivery-config /etc/nats-hub/delivery.json` for retention limits).
3. Restart both units. Check with `scripts/dogfood_durable.sh` on a staging
   box, or `nats stream ls` (shows `HUB_INGRESS`, `HUB_INBOX`, `HUB_TASKS`).

Run **one hub-server per hub**: a second one routes correctly (they share
the `router` consumer), but it can't share the embedded DB. See
[`DELIVERY.md` → Several hub-servers](DELIVERY.md#several-hub-servers).

## Verification checklist

Run every one of these on a fresh hub. Each one should succeed; if not,
follow the pointer in the right column.

| # | Check | Command | On failure, see |
|---|---|---|---|
| 1 | NATS unit is active | `systemctl is-active nats-hub-nats.service` → `active` | §8, journalctl |
| 2 | hub-server unit is active | `systemctl is-active nats-hub-server.service` → `active` | §8, journalctl |
| 3 | NATS listens on 4222 (loopback) | `ss -lntp \| grep 4222` | bind address, §4 |
| 4 | NATS listens on 8080 (public) | `ss -lntp \| grep 8080` | websocket block, §4 |
| 5 | NATS monitoring bound to loopback | `ss -lntp \| grep 8222` shows `127.0.0.1` | `http_host` setting |
| 6 | TLS cert is valid for your hostname | `openssl x509 -in /etc/nats/tls/nats-server.crt -noout -text \| grep -A1 'Subject Alternative Name'` | §3, regenerate with right SAN |
| 7 | Firewall denies 4222 from outside | from another host: `nc -zv hub.example.com 4222` → refused/timeout | §5 |
| 8 | Firewall allows 8080 from outside | from another host: `nc -zv hub.example.com 8080` → open | §5 |
| 9 | AuthN rejects anonymous | connect with no credentials → `Authorization Violation` | §4 authN block |
| 10 | Identity is bound | as user `alice`, publish `hub.pub.bob.x` → permission denied; send forged `meta.from` → arrives rewritten | `scripts/dogfood_identity.sh` |
| 10b | Inbox is private | user `alice` subscribe `channel.inbox.bob` → permission denied | `scripts/dogfood_identity.sh` |
| 10c | Replies are private | user `alice` subscribe `_INBOX.bob.>` or `_INBOX.>` → permission denied; the supervisor still answers `hub.worker.list` | `scripts/dogfood_prod_config.sh` (whole generated config, end to end) |
| 11 | Round-trip from a remote agent | run the `JOIN_HUB.md` "Verifying your connection" snippet | [`JOIN_HUB.md`](JOIN_HUB.md) |
| 12 | Logs are writing | `ls -lh /var/log/nats-hub/` shows growing file | perms on log dir, §2 |
| 13 | Backups scheduled | see [Backup](#backup) below | — |

## Onboarding a new team member

For each new agent:

1. Mint its entry: `hub-admin add-agent <identity> --role worker` (add
   `--nkey` for key-based creds, `--channel <ch>` per broadcast channel,
   `--ws-only` for WebSocket-only credentials).
2. Paste the printed `users[]` entry into `/etc/nats/nats-server.conf` — or,
   if you maintain `agents.txt`, add a line and re-run `render-config`.
3. Reload NATS:
   ```bash
   sudo systemctl reload nats-hub-nats.service   # or restart
   ```
4. Hand the user their credential (and nkey *seed* if `--nkey` was used —
   the seed is printed once as a `# SEED:` comment) and the URL; send them
   to [`JOIN_HUB.md`](JOIN_HUB.md).

To **revoke**, delete their `users[]` entry and reload.

Identity names must be single NATS tokens (`[A-Za-z0-9_-]+`) and may not be
`wave`, `session`, `agent`, `history`, `thread`, `envelope`, `stats`, or
`ping` — those collide with API op namespaces — nor the reserved hub
component names `hub-server`, `hub-orchestrator`, `visualizer`; `hub-admin`
refuses them all.

## Backup

The only persistent state is the RocksDB at `/var/lib/nats-hub/nats_hub.db`.

### Hot backup (recommended): `backup.export` + `hub-server backup`

`hub-server` exposes a `backup.export` query-API op that asks the **running**
hub-server to write a consistent `.surql` snapshot of its database (the export
runs in a single read transaction). It's a filesystem write on the hub host,
so two things gate it:

- **It's off until the operator picks a directory.** Start hub-server with
  `--backup-dir /var/backups/nats-hub`. Callers only choose a file *name*
  inside it (`[A-Za-z0-9._-]`, no `/`, not starting with `.`), never a path.
- **It's admin-only.** Run it with an identity listed in `--api-admin` (or
  `NATS_HUB_API_ADMINS`):

```bash
hub-server backup --identity ops                     # name: hub-<UTC timestamp>.surql
hub-server backup --identity ops --name before-upgrade.surql
```

`--identity` defaults to `NATS_HUB_IDENTITY`. On a hub with no `--api-admin`
and no `--require-bound-identity`, `backup.export` is unauthenticated like
every other op (still confined to `--backup-dir`). Set `--api-admin` before
exposing this on a real deployment. The export refuses to overwrite an
existing file.

Restore is offline and writes a **fresh** DB path — the export carries its
own schema, so the target must be empty:

```bash
sudo systemctl stop nats-hub-server.service          # only if restoring over the live path
hub-server restore --db-path /var/lib/nats-hub/nats_hub.db --from /var/backups/nats-hub/hub-2026-10-21-0300.surql
sudo systemctl start nats-hub-server.service         # or point a new server at the restored path
```

`restore` prints per-table row counts (envelopes, agents, sessions, waves,
wave_tasks). Everything comes back: message history with reply threads, the
agent registry, sessions, waves and wave tasks.

To restore into the *same* path as the running hub: stop the service, move the
old DB aside, restore, start. To restore alongside it (verify a backup):
restore into a scratch path and run `hub-history --db-path`… wait — history
goes through the query API, so spin a second `hub-server --db-path <scratch>`
on a spare port instead.

Schedule it with the shipped systemd timer (`deploy/systemd/`):

```bash
sudo install -m 0644 deploy/systemd/nats-hub-backup.{service,timer} /etc/systemd/system/
sudo install -d -m 0750 -o nats-hub -g nats-hub /var/backups/nats-hub
sudo systemctl daemon-reload && sudo systemctl enable --now nats-hub-backup.timer
```

**What's lost between backups:** anything mirrored to the DB after the last
export — recent message history, registry heartbeats, session/wave state
changes. With `--durable`, JetStream also buffers *undelivered* DMs and recent
task traffic, but JetStream is a **delivery buffer, not the archive**: it is
not exported and not restorable through this path. A restore means "every
committed record as of backup time"; in-flight delivery state may be redelivered
by JetStream (dedupe by `meta.id` keeps it safe).

### Cold snapshot (alternative)

```bash
sudo systemctl stop nats-hub-server.service
sudo tar -czf "/var/backups/nats-hub-$(date +%F).tar.gz" /var/lib/nats-hub
sudo systemctl start nats-hub-server.service
```

Also back up `/etc/nats/nats-server.conf` — losing the user list means
re-issuing every credential. Keep at least 14 days of backups either way.

## Rotation

- **TLS cert** — renew before expiry (ACME does this automatically). After
  renewal, copy the new files into `/etc/nats/tls/` and
  `sudo systemctl reload nats-hub-nats.service`.
- **Token (if used)** — `openssl rand -hex 32`, replace in config, reload
  NATS, redistribute to every client. Coordinate; everyone disconnects briefly.
- **User password** — edit that one entry, reload. Only that user has to update.

## Common operations

```bash
sudo systemctl status nats-hub-nats.service
sudo systemctl status nats-hub-server.service
sudo journalctl -u nats-hub-nats.service   -f --no-pager
sudo journalctl -u nats-hub-server.service -f --no-pager

# Hot reload on config change (NATS supports SIGHUP for some settings):
sudo systemctl reload nats-hub-nats.service
```

## Where to go next

- Plain-language security model: [`SECURITY.md`](SECURITY.md)
- End-user join guide (send this to your team): [`JOIN_HUB.md`](JOIN_HUB.md)
- Config blocks and Python client snippets: [`REMOTE_AGENTS.md`](REMOTE_AGENTS.md)
- systemd install detail: [`deploy/systemd/README.md`](../deploy/systemd/README.md)
