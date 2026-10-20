# Security model — in plain language

This document explains the four layers that protect a nats-hub deployment and
why each one matters. It is written for someone setting up a hub for their team
for the first time, not for a NATS expert.

If you want the copy-paste config, jump to the
[Quick reference](#quick-reference) at the bottom. For runnable code, see
[`REMOTE_AGENTS.md`](REMOTE_AGENTS.md).

## Why four layers

A bus is a relay: any message published to a subject flows out to every
subscriber on that subject. That makes it very powerful, and also very
forgiving of mistakes — if you don't put guardrails up, a misconfigured
client can read everyone's mail. So we stack four layers, each doing a
different job:

```
                            ┌─────────────────────────────────────────┐
   Internet / VPN  ───────▶ │  Layer 1: Network exposure             │
                            │  "Who can even see the port?"          │
                            ├─────────────────────────────────────────┤
                            │  Layer 2: Authentication (authN)        │
                            │  "Are you who you say you are?"         │
                            │  (token  OR  username+password)        │
                            ├─────────────────────────────────────────┤
                            │  Layer 3: Authorization (authZ)         │
                            │  "What subjects can you pub/sub?"       │
                            │  (per-user allowlists)                  │
                            ├─────────────────────────────────────────┤
                            │  Layer 4: Transport encryption (TLS)    │
                            │  "Is the wire unreadable to snoops?"    │
                            └────────────────────┬────────────────────┘
                                                 │
                                                 ▼
                                        ┌────────────────┐
                                        │  NATS server    │
                                        │  (the bus)      │
                                        └────────────────┘
```

Each layer is independent and you should use **all four** for any deployment
that is reachable from outside a single trusted machine.

---

## Layer 1: Network exposure

**Idea:** don't put ports on the public internet unless you have to.

A freshly installed nats-hub has **two listeners**:

| Port | Listener | Who should reach it |
|---|---|---|
| `4222` | Plain NATS TCP | Only `hub-server` on the **same host** (or VPN/LAN clients). Never the open internet. |
| `8080` | WebSocket gateway | The one remote agents connect to. Behind TLS it becomes `wss://`. |
| `8222` | Monitoring (`/varz`) | Loopback only. If you need to scrape it, do it through a reverse proxy with auth. |

**Firewall rules of thumb** (see `OPERATOR_HUB.md` for the exact `ufw`/`iptables`
commands):

- `8080/tcp` — open to wherever your team connects from. If everyone is on a
  VPN, only open it on the VPN interface.
- `4222/tcp` — keep on `127.0.0.1` or, at most, the LAN/VPN interface. Never
  the public interface.
- `8222/tcp` — bind to `127.0.0.1` and leave it there.

**Why this matters:** even with strong auth, leaving the plain TCP port on
the internet invites constant brute-force scanning and traffic you didn't ask
for. Tighten the surface area first.

---

## Layer 2: Authentication (authN)

**Idea:** prove who is connecting.

NATS supports several authN mechanisms. nats-hub ships config examples for the
two simplest:

### Token — one shared secret

```conf
authorization {
    token: "REPLACE_WITH_LONG_RANDOM_TOKEN"
}
```

Every client passes the same token. Simple, works, but has two real costs:

- **Revoking one person means revoking everyone.** A leaver or compromised
  laptop means rotating the token everywhere at once.
- **All clients look identical** in the logs. You can't tell who did what.

Token is fine for a small team where "team" is the trust boundary.

### Users — one credential per agent

```conf
authorization {
    users: [
        {
            user:     "remote-agent-1"
            password: "REPLACE_WITH_LONG_RANDOM_PASSWORD"
            allowed_connection_types: ["WEBSOCKET"]
        }
    ]
}
```

Each agent gets its own `user`/`password`. Costs a bit more setup, buys you:

- **Per-agent revocation.** Disable just one person, leave everyone else alone.
- **Auditability.** Logs name the connecting user.
- **Connection-type restriction.** `allowed_connection_types:["WEBSOCKET"]`
  means that credential can only be used over the WebSocket port — even if it
  leaked, it can't be reused against the plain TCP listener.
- **Identity binding.** With the bound subjects in Layer 3, the credential
  *is* the identity: `user: "echo-1"` can only publish subjects carrying
  `echo-1`. Mint entries with `hub-admin add-agent <id>`; nkey credentials
  (`--nkey`) replace the password entirely.

For anything bigger than a close-knit team, prefer `users`. For a large
multi-tenant deployment, step up further to **NKEY/JWT** (mentioned in
[`REMOTE_AGENTS.md`](REMOTE_AGENTS.md) — out of scope for this doc).

**Either way:** secrets live in `/etc/nats/nats-server.conf` (0600, owned
`nats:nats`) or in a secrets manager — never in git. See
[`deploy/systemd/README.md`](../deploy/systemd/README.md) for where each
secret goes.

---

## Layer 3: Authorization (authZ) — identity bound to credentials

**Idea:** once you're connected, what are you actually allowed to do — and
can the bus *prove* who you are?

NATS permissions are **allowlists**. Default is "nothing." You explicitly list
the subjects each user may `publish` to and `subscribe` to. Anything not on the
list is rejected.

nats-hub takes this one step further: the subject space is designed so that
**a user's allowlist *is* its identity**. Every send/register/heartbeat/API
subject embeds the caller's identity as the first token after the prefix
(contract §4.1):

| Purpose | Bound subject | Legacy (self-asserted) |
|---|---|---|
| Send an envelope | `hub.pub.<identity>.<channel>` | `hub.send.<channel>` |
| Register | `hub.register.<identity>` | `hub.register` |
| Heartbeat | `hub.presence.<identity>` | `hub.presence` |
| Query API | `hub.api.<identity>.<op>` | `hub.api.<op>` |

`<identity>` is one NATS token (`[A-Za-z0-9_-]+`). The default users[] entry
for an agent — generated by `hub-admin add-agent <id>` — looks like:

```conf
permissions: {
    publish: {
        allow: [
            "hub.pub.echo-1.>"       # can only ever send AS echo-1
            "hub.register.echo-1"    # can only register itself
            "hub.presence.echo-1"    # heartbeats
            "hub.api.echo-1.>"       # query API as itself
        ]                            # (no _INBOX publish right at all)
    }
    subscribe: {
        allow: [
            "channel.inbox.echo-1"   # ONLY its own inbox — private
            "channel.task.>"         # task conversation channels
            "channel.session.>"      # multi-turn sessions
            "channel.wave.>"         # wave coordination channels
            "_INBOX.echo-1.>"        # replies to ITS OWN requests only
        ]
    }
}
```

Broadcast channels (`channel.chat` etc.) are granted per agent with
`hub-admin add-agent <id> --channel chat`. `hub-admin` refuses channels with
`*`, `>`, empty tokens or characters outside `[A-Za-z0-9_-]`, and anything
overlapping the inboxes (`inbox`, `inbox.<id>`).

**Request-reply inboxes are per identity.** Every client (Rust `HubClient`,
`ApiClient`, the router; Python `nats_connect.connect_nats`, so every worker,
bridge and the MCP server) uses the reply prefix `_INBOX.<id>` instead of the
shared `_INBOX`: `<id>` is the NATS user when it is a valid identity, else
the client identity. A user may subscribe only `_INBOX.<its-id>.>`, so no
agent can read another agent's API or RPC replies, and no agent can publish
into any `_INBOX` subject (it can't forge a reply). Users that **answer**
requests get `allow_responses: true` instead of an `_INBOX` publish right —
it lets them reply only to requests they actually received:

| Role (`hub-admin --role`) | Extra rights |
|---|---|
| `worker` | none |
| `orchestrator`, `admin` | publish `hub.worker.>` (worker-supervisor requests) |
| `service` (worker supervisor) | publish + subscribe `hub.worker.>`, subscribe `hub.presence.*` / `hub.register.*`, `allow_responses` |
| `hub-server` (generated) | publish `channel.>`, `hub.>`; subscribe `hub.>`, `channel.>`, `_INBOX.hub-server.>`; `allow_responses` |

The **hub-server user** is shared by the router, query API, wave orchestrator
(identity `hub-orchestrator`) and WS bridge. It must be trusted: it routes
every envelope (`hub.pub.*.>` already shows it all traffic, so the
`channel.>` subscribe the orchestrator needs for `channel.wave.>` adds no
visibility) and can publish on any delivery subject. The identities
`hub-server`, `hub-orchestrator` and `visualizer` are **reserved**:
`hub-admin` won't mint agent credentials for them, and `hub-server` refuses a
`--ws-identity` of `hub-server`/`hub-orchestrator`.

**What the NATS ACL alone cannot express:** "all channels except inboxes"
(a `deny` always beats the broad `channel.>` allow). So task, session, and
wave channels are visible to every agent — treat them as team-public — while
each `channel.inbox.<id>` is private by construction.

### What the hub enforces on top of NATS ACLs

The router enforces the following in every mode:

- **`meta.from` and `meta.channel` are overwritten.** On a bound
  `hub.pub.<id>.<channel>` subject the router rewrites `meta.from` to `<id>`
  and `meta.channel` to `<channel>` before routing and storage — a forged
  field cannot survive the bus. Same for the identity inside
  `hub.register.<id>` / `hub.presence.<id>` payloads.
- **Recipients are validated.** An envelope whose `meta.to` is not a valid
  identity is dropped (logged). A send on channel `inbox.<id>` without
  `meta.to` is treated as the DM to `<id>` it is (so it is stored and scoped
  as private, not public). Legacy register/presence payload identities are
  validated too.

The router and query API add the rest when `hub-server` runs with
`--require-bound-identity` or `--api-admin`:

- **Legacy subjects are dropped** under `--require-bound-identity`
  (`hub.send.>`, bare `hub.register`/`hub.presence`, `hub.api.<op>`).
  Without the flag they still work and are counted in
  `natshub_unbound_sends_total` — the metric tells you when it's safe to
  flip the flag.
- **A legacy `hub.api.<op>` caller owns nothing once `--api-admin` is set**
  (even without `--require-bound-identity`): it can't write, sees only
  broadcast envelopes and unscoped metadata (`agent.*`, `stats.*`, `ping`),
  and gets no waves, sessions or pending inboxes. Only a hub with neither
  flag treats legacy callers as privileged.
- **`hub.api.<id>.<op>` scopes reads to the caller.** DM envelopes are
  visible only to their sender and recipient (`history.query`,
  `thread.get`, `envelope.get`, `thread.pending`); broadcast envelopes are
  public; wave and session records are visible to their orchestrator and
  workers. `envelope.get` on an invisible record returns "envelope not
  found" — existence is not leaked. `history.query`/`thread.get` over-fetch
  before filtering, so a page is not short just because rows the caller
  can't see filled the `LIMIT`.
- **Write ops are owner-scoped; admins can do anything.** A non-admin may
  mutate only what it owns. Admins are listed via `--api-admin <id>`
  (repeatable) or `NATS_HUB_API_ADMINS`; they bypass write checks and read
  scoping.
  - `wave.create`: the new wave's `orchestrator` must be the caller, and an
    existing wave with that id must already be the caller's.
  - `wave.create_task` / `spawn` / `cancel` / `update_status`: the caller must
    orchestrate the wave.
  - `wave.update_task_status`: the wave's orchestrator, or the task's worker.
  - `session.create`: the new session's `orchestrator` must be the caller, and
    an existing session with that id must already be the caller's.
  - `session.update_status` / `set_backend_ctx`: the session's orchestrator or
    worker.
  - Any other write op is admin-only. Implemented in
    `src/query_api/write_authz.rs`. Storage backs the create rule up: a
    wave/session create never changes an existing record's orchestrator
    (checked in the same transaction as the write — not even an admin can
    re-own a record through `create`).
- **Bound subjects still work in permissive mode.** Clients always publish
  bound subjects; enforcement is what changes.

Identities may not collide with API op namespaces (`wave`, `session`,
`agent`, `history`, `thread`, `envelope`, `stats`, `ping`) — `hub-admin`
refuses to mint them, and the API's subject parser would otherwise read
`hub.api.wave.get` as legacy op `wave.get`.

### Durable delivery (`hub-server --durable`)

JetStream keeps DMs, task/session/wave channels and in-flight sends on disk
(see [`DELIVERY.md`](DELIVERY.md)). Durable mode keeps the same inbox
privacy:

- **Agents can't create JetStream objects.** hub-server creates each
  identity's `inbox-<id>` pull consumer when the agent asks on
  `hub.delivery.<id>.inbox` (a bound subject). The agent may only look up,
  pull from and ack **its own** consumer. A creatable consumer would be a
  bypass, because nats-server doesn't check a push consumer's
  `deliver_subject` against the creator's permissions.
- **Stored DMs can't be read by sequence.** Agents get no
  `STREAM.MSG.GET`, `DIRECT.GET` or `STREAM.INFO` on `HUB_INBOX`.
- **Replays are dropped.** JetStream doesn't permission-check API reply
  subjects either, so an agent could aim a `DIRECT.GET` reply at another
  identity's ingress subject. The durable router drops ingress messages
  that carry JetStream delivery headers or the router's own message id, or
  that arrive on a non-ingress subject.
- **Task/session/wave channels are readable by sequence**
  (`DIRECT.GET.HUB_TASKS`). This is the same visibility `channel.task.>`
  already gives every agent (see residual risks below), now extended over
  the 24 h retention.

`tests/delivery_permissions.rs` and `scripts/dogfood_durable.sh` prove this
on a `hub-admin render-config --durable` config.

### Honest threat model — what is and isn't proved

| Deployment mode | Who can claim `meta.from = "alice"`? | Can alice read bob's inbox or replies? | `hub.api` visibility |
|---|---|---|---|
| Anonymous / shared token, permissive (default) | **Anyone** — `from` is self-asserted on legacy subjects; bound subjects get overwritten but nothing pins identity to a credential | **Anyone** — no ACLs, so `channel.inbox.>` and `_INBOX.>` are open | Unscoped; bound callers scoped only if `--api-admin`/`--require-bound` set |
| Per-agent users (`hub-admin`) + `--require-bound-identity` (+ `--api-admin`) | **Only alice's credential** — publish ACL allows `hub.pub.alice.>` only, legacy subjects dropped, `meta.from` overwritten | **No** — bob's credential alone may subscribe `channel.inbox.bob` and `_INBOX.bob.>` | Scoped to caller; writes limited to waves/sessions the caller owns (admins: `--api-admin`) |

So the claim holds only when **both** halves are true: per-agent
credentials (NATS layer) *and* `hub-server --require-bound-identity` (hub
layer). In permissive mode the sender checks workers and delegators make
(accept a cancel only from the task's sender, a result only from the
delegated worker) compare a self-asserted `meta.from`, so they are best
effort; in bound mode they are real. Two scripts prove the guarantees live:
`scripts/dogfood_identity.sh` (bound subjects, API scoping) and
`scripts/dogfood_prod_config.sh` (the full generated config: inbox and
reply isolation, forged `from`, supervisor responses, a wave run and a
wave cancel, with every process on its own credential). The prod config in
`config/nats-server.prod.conf.example` is the reference deployment.

**Residual risks, even in the strict mode** — these are *not* guarantees:

- **Workload channels are team-public.** `channel.task.>`,
  `channel.session.>` and `channel.wave.>` are readable by every agent: any
  agent can read the prompts, progress and results of tasks, sessions and
  waves it isn't part of. NATS ACLs can't scope them per task.
- **Any agent can post on any channel as itself**, including someone
  else's task/session/wave channel (`hub.pub.<self>.task.<uuid>` is allowed).
  `meta.from` stays truthful, so receivers that check the sender (the wave
  orchestrator, workers, delegators) ignore it; consumers that don't check
  can be confused. Any agent can also DM any other agent.
- **Broadcasts are public history.** Every caller sees every broadcast
  envelope through `history.query`, and `agent.*`/`stats.*` are unscoped.
- **The hub-server credential is all-powerful** (it routes and stores
  everything), and so is any process holding it — keep it TCP-local.
- **The WS bridge acts as one identity** (`--ws-identity`) for everyone who
  holds the bridge token; browser users are not individually authenticated.
- **A leaked credential is a full identity takeover** until it is revoked.
  With `--durable` it also gets the DMs that are still stored for that
  identity (up to the inbox retention, 7 d by default).
- **Stored data is plaintext on the nats-server host** (`jetstream.store_dir`)
  in durable mode. Protect the host and its backups like the SurrealDB file.

Inbox and reply privacy, `from`/`channel` integrity and ownership of
waves/sessions are the guarantees; workload-channel confidentiality is not.

---

## Layer 4: TLS (transport encryption)

**Idea:** make the wire itself unreadable.

Without TLS, every message — including the token or password — crosses the
network in cleartext. Anyone on the path (coffee-shop Wi-Fi, a hostile cloud
network, a compromised router) can read it.

**For the WebSocket listener** (`wss://`), put a cert + key in the
`websocket {}` block:

```conf
websocket {
    port: 8080
    cert_file: "/etc/nats/tls/nats-server.crt"
    key_file:  "/etc/nats/tls/nats-server.key"
    same_origin: false
}
```

Generating the cert — pick one:

- **Self-signed / private CA** — fine for internal teams, less polished.
  One-liner in [`REMOTE_AGENTS.md`](REMOTE_AGENTS.md) § "Production Setup",
  or see [`OPERATOR_HUB.md`](OPERATOR_HUB.md) § "TLS certificate".
  You must ship the CA/cert file to every client.
- **Public CA (ACME / Let's Encrypt)** — preferred for anything
  internet-facing. Clients use the system trust store, nothing extra to ship.

For the plain TCP listener, you can add a top-level `tls {}` block the same
way if you expose `:4222` beyond loopback. We recommend not exposing it at all.

**One-sided TLS is enough.** Client-cert (mTLS) is supported but adds
operational complexity. Token or user/password authN combined with
server-side TLS already gives you confidentiality + authentication.

---

## The visualizer's WS bridge (`hub-server --ws-addr`)

The visualizer bridge is a second door into the hub, separate from NATS
auth: it reads **all** bus traffic and publishes commands that can spawn
workers (`ensure_worker` → `worker_supervisor.py` starts always-approve
agents). Four controls protect it; use them together whenever the port is
reachable by anything but localhost.

- **Origin allowlist.** Browsers disclose which site opened the socket via
  the `Origin` header, and browsers are the real threat here — any web page
  you visit could otherwise open `ws://127.0.0.1:9191/ws` and read or drive
  the bus. `http://<ws-addr>` is always allowed; a loopback or wildcard
  bind also allows every loopback spelling (`localhost`/`127.0.0.1`/`[::1]`),
  and a routable bind adds its resolved IPs plus `localhost`.
  `--ws-allow-origin` (repeatable) adds entries — matching ignores case,
  trailing `/` and default ports. A mismatched or unparsable Origin gets
  HTTP 403 before the upgrade. Requests with **no** `Origin` header (curl,
  scripts, the NATS-clients) are not checked — the token below is what
  gates them.
- **Token.** Env `HUB_WS_TOKEN` (preferred — `--ws-token` is visible in
  `ps`) requires `?token=` on the WS URL (HTTP 401 otherwise). Open the
  visualizer at `http://<addr>/?token=T` and the page forwards it to `/ws`
  itself. hub-server prints the tokenized URL at startup (percent-encoded,
  so `+`/`=`/`/` in tokens are safe). Rotate the token like any other
  shared secret; it is not a per-user credential.
- **Loopback discipline.** With no token configured, `--ws-addr` must
  resolve to loopback (`127.0.0.1`, `::1`, `localhost`), otherwise
  hub-server refuses to start. `--ws-insecure` overrides — for trusted
  LAN/VPN binds only.
- **Static root confinement.** The static file server percent-decodes the
  request path, rejects `..` components and NUL bytes (raw or encoded), and
  canonicalizes the result — symlink escapes included — refusing anything
  that lands outside `--static-dir`.

---

## Quick reference

| Concern | Setting |
|---|---|
| Bind plain TCP to loopback | `host: "127.0.0.1"` |
| Bind monitoring to loopback | `http_host: "127.0.0.1"` |
| Open port to the world | `8080/tcp` only; everything else VPN/LAN |
| Auth: transitional | `authorization { token: "..." }` — identity NOT bound; hub stays permissive |
| Auth: per-agent (required for binding) | `authorization { users: [...] }` via `hub-admin render-config` |
| Identity binding | `hub-server --require-bound-identity` + per-agent users |
| API write ops | Owner-scoped by default; full access via `hub-server --api-admin <id>` (repeatable) or `NATS_HUB_API_ADMINS` |
| Per-agent authZ | identity-bound allowlists — `hub.pub.<id>.>`, `channel.inbox.<id>`, `_INBOX.<id>.>` — mint via `hub-admin add-agent` |
| Responders (supervisor) | `hub-admin --role service` → `allow_responses: true` (no `_INBOX` publish) |
| TLS on WS | `websocket { cert_file, key_file }` |
| Visualizer bridge | `--ws-token` (or `HUB_WS_TOKEN`) + Origin allowlist; loopback-only without a token |
| Capacity ceilings | `max_connections`, `max_payload` |
| Secrets out of git | `/etc/nats/nats-server.conf` 0600 `nats:nats`, or secrets manager |

## Where to go next

- **Stand up a hub from scratch:** [`OPERATOR_HUB.md`](OPERATOR_HUB.md)
- **Join an existing hub as a remote agent:** [`JOIN_HUB.md`](JOIN_HUB.md)
- **Copy-paste config blocks and Python client snippets:** [`REMOTE_AGENTS.md`](REMOTE_AGENTS.md)
