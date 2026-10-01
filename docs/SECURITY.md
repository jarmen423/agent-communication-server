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
            "_INBOX.>"               # request-reply plumbing
        ]
    }
    subscribe: {
        allow: [
            "channel.inbox.echo-1"   # ONLY its own inbox — private
            "channel.task.>"         # task conversation channels
            "channel.session.>"      # multi-turn sessions
            "channel.wave.>"         # wave coordination channels
            "_INBOX.>"               # replies to its API calls
        ]
    }
}
```

Broadcast channels (`channel.chat` etc.) are granted per agent with
`hub-admin add-agent <id> --channel chat`.

**What the NATS ACL alone cannot express:** "all channels except inboxes"
(a `deny` always beats the broad `channel.>` allow). So task, session, and
wave channels are visible to every agent — treat them as team-public — while
each `channel.inbox.<id>` is private by construction.

### What the hub enforces on top of NATS ACLs

The router and query API add a second enforcement layer, active whenever
`hub-server` runs with `--require-bound-identity` or `--api-admin`:

- **`meta.from` is overwritten.** On a bound `hub.pub.<id>.>` subject the
  router rewrites the envelope's `meta.from` to `<id>` before routing — a
  forged `from` field cannot survive the bus. Same for the identity inside
  `hub.register.<id>` / `hub.presence.<id>` payloads.
- **Legacy subjects are dropped** under `--require-bound-identity`
  (`hub.send.>`, bare `hub.register`/`hub.presence`, `hub.api.<op>`).
  Without the flag they still work (permissive mode) and are counted in
  `natshub_unbound_sends_total` — the metric tells you when it's safe to
  flip the flag.
- **`hub.api.<id>.<op>` scopes reads to the caller.** DM envelopes are
  visible only to their sender and recipient (`history.query`,
  `thread.get`, `envelope.get`, `thread.pending`); broadcast envelopes are
  public; wave and session records are visible to their orchestrator and
  workers. `envelope.get` on an invisible record returns "envelope not
  found" — existence is not leaked.
- **Write ops are admin-only.** `wave.*` / `session.*` mutations require
  the caller identity to be listed via `--api-admin <id>` (repeatable) or
  `NATS_HUB_API_ADMINS`. Admins bypass read scoping.
- **Bound subjects still work in permissive mode.** Clients always publish
  bound subjects; enforcement is what changes.

Identities may not collide with API op namespaces (`wave`, `session`,
`agent`, `history`, `thread`, `envelope`, `stats`, `ping`) — `hub-admin`
refuses to mint them, and the API's subject parser would otherwise read
`hub.api.wave.get` as legacy op `wave.get`.

### Honest threat model — what is and isn't proved

| Deployment mode | Who can claim `meta.from = "alice"`? | Can alice read bob's inbox? | `hub.api` visibility |
|---|---|---|---|
| Anonymous / shared token, permissive (default) | **Anyone** — `from` is self-asserted on legacy subjects; bound subjects get overwritten but nothing pins identity to a credential | **Anyone** — default ACLs allow `channel.inbox.>` | Unscoped legacy callers; bound callers scoped only if `--api-admin`/`--require-bound` set |
| Per-agent users + `--require-bound-identity` (+ `--api-admin`) | **Only alice's credential** — publish ACL allows `hub.pub.alice.>` only, legacy subjects dropped, `meta.from` overwritten | **No** — ACL allows only `channel.inbox.bob` for bob's credential | Scoped to caller; writes need `--api-admin` |

So the claim holds only when **both** halves are true: per-agent
credentials (NATS layer) *and* `hub-server --require-bound-identity` (hub
layer). `scripts/dogfood_identity.sh` proves each guarantee live against a
running hub; the prod config in `config/nats-server.prod.conf.example` is
the reference deployment.

**Residual risk even in the strict mode:** task/session/wave channel names
are public to all agents (a worker can eavesdrop on a task channel it
wasn't assigned to — ACLs can't express per-task scoping), and a leaked
credential is still a full identity takeover until revoked. Inbox privacy
and `from` integrity are the guarantees; workload-channel confidentiality
is not one of them.

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
  One-liner in [`OPERATOR_AGENTS.md`](REMOTE_AGENTS.md) § "Production Setup",
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
| API write ops | `hub-server --api-admin <id>` (repeatable) or `NATS_HUB_API_ADMINS` |
| Per-agent authZ | identity-bound allowlists — `hub.pub.<id>.>`, `channel.inbox.<id>` — mint via `hub-admin add-agent` |
| TLS on WS | `websocket { cert_file, key_file }` |
| Visualizer bridge | `--ws-token` (or `HUB_WS_TOKEN`) + Origin allowlist; loopback-only without a token |
| Capacity ceilings | `max_connections`, `max_payload` |
| Secrets out of git | `/etc/nats/nats-server.conf` 0600 `nats:nats`, or secrets manager |

## Where to go next

- **Stand up a hub from scratch:** [`OPERATOR_HUB.md`](OPERATOR_HUB.md)
- **Join an existing hub as a remote agent:** [`JOIN_HUB.md`](JOIN_HUB.md)
- **Copy-paste config blocks and Python client snippets:** [`REMOTE_AGENTS.md`](REMOTE_AGENTS.md)
