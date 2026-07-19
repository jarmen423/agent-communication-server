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

For anything bigger than a close-knit team, prefer `users`. For a large
multi-tenant deployment, step up further to **NKEY/JWT** (mentioned in
[`REMOTE_AGENTS.md`](REMOTE_AGENTS.md) — out of scope for this doc).

**Either way:** secrets live in `/etc/nats/nats-server.conf` (0600, owned
`nats:nats`) or in a secrets manager — never in git. See
[`deploy/systemd/README.md`](../deploy/systemd/README.md) for where each
secret goes.

---

## Layer 3: Authorization (authZ)

**Idea:** once you're connected, what are you actually allowed to do?

NATS permissions are **allowlists**. Default is "nothing." You explicitly list
the subjects each user may `publish` to and `subscribe` to. Anything not on the
list is rejected.

The default shape we ship for a remote worker covers the full bus experience
without giving it more than it needs:

```conf
permissions: {
    publish: {
        allow: [
            "hub.send.>"      # send a message to any channel (router re-scopes it)
            "hub.register"    # register this agent with the registry
            "hub.presence"    # heartbeats
            "hub.events.>"    # structured progress events
        ]
    }
    subscribe: {
        allow: [
            "channel.inbox.>"    # private messages routed to me
            "channel.task.>"     # task conversation channels
            "channel.session.>"  # multi-turn sessions
            "channel.wave.>"     # wave coordination channels
        ]
    }
}
```

**Tightening per agent.** When you can, narrow the wildcards to the agent's
identity. For example, if `remote-agent-1` should only ever receive DMs on its
own inbox, swap `channel.inbox.>` for `channel.inbox.remote-agent-1`. The
template above is the **widest reasonable** default; treat it as a ceiling,
not a target. See [`REMOTE_AGENTS.md`](REMOTE_AGENTS.md) for a per-agent
example.

**Why this matters even with authN.** If an attacker steals one worker's
credential, authZ limits the blast radius — they still can't, say, subscribe
to `channel.inbox.some-other-agent` or publish on `hub.api.>` (which lives on
the server's trusted localhost connection).

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

## Quick reference

| Concern | Setting |
|---|---|
| Bind plain TCP to loopback | `host: "127.0.0.1"` |
| Bind monitoring to loopback | `http_host: "127.0.0.1"` |
| Open port to the world | `8080/tcp` only; everything else VPN/LAN |
| Auth: small team | `authorization { token: "..." }` |
| Auth: per-agent | `authorization { users: [...] }` with `allowed_connection_types:["WEBSOCKET"]` |
| Per-agent authZ | `permissions.publish.allow` + `permissions.subscribe.allow` allowlists |
| TLS on WS | `websocket { cert_file, key_file }` |
| Capacity ceilings | `max_connections`, `max_payload` |
| Secrets out of git | `/etc/nats/nats-server.conf` 0600 `nats:nats`, or secrets manager |

## Where to go next

- **Stand up a hub from scratch:** [`OPERATOR_HUB.md`](OPERATOR_HUB.md)
- **Join an existing hub as a remote agent:** [`JOIN_HUB.md`](JOIN_HUB.md)
- **Copy-paste config blocks and Python client snippets:** [`REMOTE_AGENTS.md`](REMOTE_AGENTS.md)
