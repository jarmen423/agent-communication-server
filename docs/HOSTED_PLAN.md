# Hosted ACS — plan

**Status:** draft, 2026-10-20. Nothing here is built yet except the parts the
"What we already have" table marks ✅. Decisions still open are listed at the
end.

## 1. What we're building

A user signs up for an **ACS account**. We run their hub. They connect any
machine in a minute, the way you add a machine to Tailscale:

```
$ acs login            # opens the browser: device-code sign-in
$ acs up               # background daemon: keeps credentials fresh
$ acs mcp install claude   # wires the MCP plugin to this account
```

From then on, every agent on every machine they've connected can delegate to,
DM and run waves with every other agent in their account, with durable delivery
when laptops sleep. Self-hosting keeps working with the same binaries (see
section 9).

### Kinds of agents, and how each connects

ACS is the connector for *all* of a user's agents, not only coding CLIs:

| Agent kind | Examples | Connects through | Can act as |
|---|---|---|---|
| Local CLI agents | Claude Code, Codex, Cursor, Hermes | `acs` + the local (stdio) MCP plugin, or a worker | orchestrator **and** worker |
| Personal agents / bots running as services | dots, grok bot, muse | `acs` on their host with a headless login (§4), then the MCP plugin, the Python client or `hub-worker` | orchestrator **and** worker (always on) |
| Web chat apps | claude.ai, chatgpt.com, grok.com | the **hosted remote MCP endpoint** (§4b), added as a custom connector; ACS sign-in via OAuth | orchestrator only (acts during a chat turn) |

Web chat agents can't run a binary or hold a connection open between turns,
so they can't sit waiting for tasks. They can delegate, check their inbox
and fetch results, and durable delivery (`--durable`) keeps their DMs and
task results until the next turn picks them up.

## 2. What we already have

| Need | State | Where |
|---|---|---|
| Identity bound to the credential (no forged `from`) | ✅ | Bound subjects + `--require-bound-identity`, `SECURITY.md` |
| Per-identity inbox/reply privacy, owner-scoped API | ✅ | `hub-admin` ACLs, `src/query_api/authz.rs`, `write_authz.rs` |
| Durable delivery (sleeping laptops, hub restarts) | ✅ | `hub-server --durable`, `DELIVERY.md` |
| Remote clients over `wss://` with creds files | ✅ | `NATS_CREDENTIALS_FILE`, `JOIN_HUB.md` |
| Release binaries for linux/macOS | ✅ | `release.yml`, `install_remote.sh` |
| Many tenants on one NATS deployment, isolated | ⬜ | Section 3 |
| Credentials issued by an account service, not a config file | ⬜ | Section 3 |
| `acs` connector (login, daemon, MCP install) | ⬜ | Section 4 |
| Provisioning a hub-server per tenant | ⬜ | Section 5 |
| Account service, billing, dashboard | ⬜ | Section 6 (private) |
| Hosted ops (cluster, TLS, backups, limits, monitoring) | ⬜ | Section 7 |

## 3. Tenancy and auth

### Shape

```
                       ┌──────────── ACS control plane (private) ────────────┐
 browser ── sign up ──▶│ accounts · tenants · devices · identities · billing │
                       │ holds each tenant's account signing keys (KMS)      │
                       └───────┬───────────────────────────┬─────────────────┘
               push account JWT│                           │ deploy / scale
                               ▼                           ▼
   ┌──────────── NATS cluster (operator mode, 3 nodes) ┐   ┌ per-tenant hub-server ┐
   │  account T_acme   account T_globex   ...          │◀──│ --durable             │
   │  (own subjects, own JetStream quota, own streams) │   │ --require-bound-id    │
   └──────────▲────────────────────────────────────────┘   │ SurrealDB on a volume │
              │ wss://connect.acs…  (user JWT + nkey)       └───────────────────────┘
      laptop: acs daemon → creds → MCP / workers
```

- **One NATS account per tenant.** Accounts are NATS's isolation boundary:
  subjects, streams and consumers in one account are invisible to every other.
  Each tenant gets the same `hub.*`/`channel.*` subjects and the same `HUB_*`
  stream names. Nothing in our code needs a tenant prefix.
- **Operator mode (decentralized JWT auth).** We hold the operator key offline.
  The control plane creates each tenant's account JWT (with JetStream limits:
  storage, streams, consumers, connections) and pushes it to the cluster's
  built-in account resolver. No config file per tenant and no server restart.
- **Credentials are user JWTs signed by the tenant's account signing key.**
  The JWT's name is the agent identity. Its permissions are exactly what
  `hub-admin` renders today (`src/bin/hub_admin/acl.rs`). Every entry there
  is either constant or parameterized by the identity alone (`hub.pub.<id>.>`,
  `_INBOX.<id>.>`, `$JS.API.CONSUMER.MSG.NEXT.HUB_INBOX.inbox-<id>`, …).
  That lets us use **scoped signing keys**: one key per role (worker,
  orchestrator, admin) whose permission template uses `{{name()}}`. Changing
  a template updates every connected user immediately.
- **The tenant's hub-server** connects as a hub-server user in that account
  only. The "hub-server credential is all-powerful" residual risk is then
  bounded by the tenant.

### Recommendation: operator mode + scoped signing keys, not auth callout

| | Operator/JWT (recommended) | Auth callout |
|---|---|---|
| Who decides on connect | nats-server, offline, from the signed JWT | our auth service, on every connect |
| Control plane down | existing creds keep working; only new logins fail | **nobody can connect** |
| Revocation | short JWT expiry + account revocation list | immediate (deny next connect) |
| Fits our ACL model | 1:1 via `{{name()}}` templates | also fine (service returns a user JWT) |
| Ops | resolver + key custody | an HA auth service on the connect path |

Callout's advantage is mapping an arbitrary external token straight to a NATS
user, so `acs` wouldn't need to hold an nkey. Its cost is that our service
sits on the connect path for every tenant. We avoid that and use short-lived
JWTs (24 h, refreshed by `acs`) for timely revocation. **The spike (H-0)
confirms this before we build on it.**

**Known gap:** `hub-admin --channels` adds per-agent extra broadcast
channels, which a role template can't express. Options: tenant-wide broadcast
channels in the template (simplest; a tenant is one team), or sign those
users with an unscoped key and explicit permissions. Decide in the spike.

### Credential lifecycle

1. `acs login` runs the OAuth device-code flow against the control plane and
   gets a **device refresh token**, stored in the OS keychain (file fallback,
   `0600`).
2. For each identity on that device (`acs agent add claude-laptop`), `acs`
   generates an **nkey locally** and sends only the public key. The control
   plane checks that the identity name is free in the tenant and not
   reserved (same rules as `check_identity`), then returns a user JWT signed
   with the role's scoped key, valid for 24 h.
3. The daemon writes `~/.config/acs/creds/<identity>.creds` and refreshes it
   before expiry. NATS clients pick up the new JWT on reconnect, and our
   clients already reconnect.
4. Revoking a device or identity: the control plane stops refreshing it and
   adds the user key to the account's revocation list (pushed immediately).

Private keys never leave the device. The control plane holds account signing
keys (KMS-backed), never user keys.

### H-0 results (2026-10-21)

The operator spike provisions **nsc v2.15.0** (pinned release, SHA256 checked)
and nats-server v2.15.0, with a full account resolver and two tenants. Both
unchanged hub-servers run durable and require bound identity. The same
`hub-admin` policy now emits config-mode allowlists and scoped signing-key
JSON (`permission-template`); a frozen-policy unit test checks every role,
including service responses and `hub-server`. No router, API, storage or
delivery changes are needed.

Observed: the production dogfood passed all 12 checks, including wave run
and cancellation; the durable dogfood passed router outage, offline worker,
consumer-permission and replay checks. Live isolation tests run both tenants
with identical subjects and stream names. Even a tenant hub-server cannot
publish into or observe the other account's `hub.*`, `channel.*`, `_INBOX.*`
or delivery traffic; worker ACLs reject unauthorized subject families and
`$SYS`. Each query API/history and `HUB_TASKS` direct read sees only its own
tenant's records. See [`OPERATOR_MODE.md`](OPERATOR_MODE.md) for reproducible
commands and the self-host setup.

The 60-second credential lifecycle test passed with the same Rust client,
Python worker process and MCP connection surviving expiry after atomic
credential refresh. An unrefreshed JWT was rejected. Pushing the account
revocation list disconnected an existing user and denied a fresh login.

**Extra channels decision:** tenant-wide channels in role templates for
hosted v1. `--channel` on `permission-template` expresses them using the
same validation as config mode. Avoid unscoped exception keys in the hosted
issuer; self-host config mode retains per-agent channel grants.

**Operator/JWT decision:** keep the proposed design. Account updates can be
pushed through the resolver and tenant clients authenticate without an
online account service. Production should mint with maintained NATS JWT/
NKey libraries and KMS-held role signing keys, accept only device-generated
public user keys, and push account claims through the system API. NSC is
only the reproducible spike/admin tool. Auth callout was not implemented or
benchmarked, so H-0 does not claim an availability or latency comparison.

**Surprises:** nats-server 2.15 expands `inbox-{{name()}}` as needed by our
existing durable consumer names. JetStream quotas must exceed the sum of
stream reservations (the three defaults reserve 1 GiB each). H-1c now derives the reply-inbox permission identity from the creds JWT's
`name`, so the tenant hub-server and its internal wave client need no
`NATS_USER=hub-server` hint.
Rust async-nats snapshots credentials by default; the shared helper now
reloads them on authentication. nats-py rereads files but closes permanently
on an expiry error; the shared helper handles that exact event by
reconnecting. Plugin copies are regenerated from the canonical helper.

## 4. The `acs` connector (public, Apache-2.0)

A single static Rust binary built on the client crate (the client/server crate
split is part of the license work).

| Command | Does |
|---|---|
| `acs login` / `logout` | device-code sign-in; stores the device refresh token |
| `acs up` / `down` | install/start the daemon (systemd user unit, launchd agent) |
| `acs status` | account, tenant, connection, identities, credential expiry |
| `acs agent add <id> [--role worker\|orchestrator]` / `rm` | mint/revoke an identity on this device |
| `acs mcp install claude\|codex\|hermes [--identity id]` | write the plugin/MCP config: `NATS_URL`, `NATS_CREDENTIALS_FILE`, `NATS_HUB_IDENTITY` |
| `acs worker run <backend> --identity id` | start a worker (wraps the existing Python workers or `hub-worker`) |

**v1:** the daemon is a credential manager. Agents connect straight to
`wss://connect.acs…` with their own creds file. Nothing on our wire protocol
changes.

**Later, maybe:** a local leaf node (`localhost:4222`) so local agents need no
creds at all. That's closer to tailscaled, but identity binding gets harder:
the leaf connection is one NATS user, so the daemon would have to enforce
per-agent subjects locally. Not for v1.

**MCP without the binary:** on a machine, v1 requires `acs` (a local MCP
server running its own device flow would duplicate the connector). Where
there is no machine at all (web chat apps), use §4b.

**Headless login** for always-on bots and servers (dots, grok bot, muse):
`acs login --headless` prints the device-code URL to open on any other
device, or `acs login --key <service key>` uses a revocable per-device service key
created by the control plane (a DEV ONLY admin endpoint in the stub). Either way the daemon then refreshes credentials
as usual.

**Built (H-1c; usage and limitations in [`ACS_CONNECTOR.md`](ACS_CONNECTOR.md)):**
`crates/acs` is an Apache-2.0 Rust workspace crate using the public client.
The DEV ONLY stub provides RFC 8628 device authorization, refresh rotation,
per-device service keys, registration/revocation and tenant/device/public-key
checks on credential issue. Device tokens target the control plane, separate
from web-MCP tokens. `acs` keeps private user nkeys local, writes credentials
atomically as `0600`, and runs a refresh daemon with jitter/backoff. It installs
Linux systemd user units and macOS launchd agents, merges client MCP configs,
and wraps existing workers. Standalone MCP installs materialize an embedded
canonical Python runtime and a private Python venv. Keychain storage uses
`keyring`, falling back to a private token file. Stub revocation stops renewal;
resolver-pushed account revocation remains a production control-plane task.

## 4b. Hosted remote MCP endpoint (for web chat apps)

`https://mcp.acs…/` speaks MCP over streamable HTTP with OAuth sign-in, so a
user adds ACS as a custom connector in claude.ai, ChatGPT or Grok and
approves it with their ACS login.

- **Each connector install is one identity** in the tenant (for example
  `claude-web`, `chatgpt-web`), minted on the OAuth consent screen.
- **The endpoint holds the NATS connection for it.** It is a multi-tenant
  service that connects into each tenant's account with that identity's
  credentials, using the same tool set as `mcp_server/` (delegate, inbox,
  `wait_for_task`, waves, sessions). Today's MCP server is stdio-only and
  keeps its buffers in memory, so the hosted version reads inboxes and task
  results from JetStream (the `inbox-<id>` consumer, `HUB_TASKS`) instead of
  process memory. That makes it stateless and horizontally scalable.
- **Turn-bounded waits.** Chat apps cap how long one tool call may run, so
  `delegate` returns a task id quickly and `wait_for_task` waits only up to
  that cap. A later turn picks up the result.
- **Per-platform facts (H-0b research, 2026-10-21; sources in
  `.planning/hosted/H-0b-web-mcp.md`):**

  | | claude.ai | ChatGPT | Grok |
  |---|---|---|---|
  | Custom remote MCP by URL | yes ("custom connectors"), Free (1) and every paid plan | yes ("custom MCP server" plugin, Work chats); write tools on Business/Enterprise/Edu, Plus/Pro unclear | yes ("custom MCP connectors"), all users |
  | Auth | OAuth per the MCP auth spec (DCR, CIMD or pre-registered), PKCE S256; no-auth allowed | OAuth 2.1 (CIMD preferred, DCR, predefined), PKCE S256; mixed per-tool auth | OAuth pop-up; details not documented |
  | Transport | streamable HTTP (SSE legacy) | streamable HTTP + SSE | not documented for grok.com |
  | Tool-call timeout | 240 s; results ~150k chars | not official (forum: ~60 s) | not documented |
  | Directory | Connectors Directory (automated scan → "Community", review → "Verified") | shared ChatGPT/Codex plugin directory, reviewed | curated catalog, no submission found |

  **What this means for the design:**
  - Build one streamable-HTTP endpoint.
  - The OAuth server needs CIMD and DCR, PKCE S256, RFC 9728 resource
    metadata, a `401` + `WWW-Authenticate` response, and refresh rotation.
  - `delegate` returns a task id within seconds. Assume about 60 s per call
    at most.
  - Results are fetched by polling (`get_task_result`, `read_inbox`) in a
    later turn.
  - Mark every read tool `readOnlyHint` so it runs without a confirmation
    prompt. Claude's directory also requires a `title` and the hint
    annotations on every tool.
  - Page long outputs.
  - Distribute through the Claude directory first, ChatGPT plugin
    submission second, and Grok/Gemini/Perplexity as custom URLs.
  - Later: ChatGPT's MCP Events (webhooks) could push results instead of
    polling.
- Code: public (Apache-2.0, it's a client) but it runs only as part of the
  hosted service.

**Built (H-1b, 2026-10-21; details in [`REMOTE_MCP.md`](REMOTE_MCP.md)):**

- **`mcp_remote/`** is the endpoint: stateless streamable HTTP, no
  `Mcp-Session-Id`. Results and session replies are read back from
  `HUB_TASKS` (`DIRECT.GET`) and the inbox from the identity's durable
  `inbox-<id>` consumer, so any replica answers any call. It requires
  `hub-server --durable`.
  - Turn-bounded tools: `delegate` returns at once; `get_task_result` waits
    ≤ 50 s and pages results at ≤ 100k characters with an offset cursor.
  - Every tool has a title and read-only/destructive hints. Shared tools
    reuse the `mcp_server/` handlers, now behind a pluggable transport
    (`hub_connection.bound`, `hub_app.build_server`).
- **OAuth.** The endpoint is the resource server: `401` + `resource_metadata`
  and RFC 9728 metadata. Access tokens are **EdDSA JWTs** verified offline
  against the control plane's JWKS, which keeps the control plane off the
  tool-call path. They are 15 min, so revocation lags by at most that.
  - The token's `acs_tenant`/`acs_identity` is the only source of identity.
  - The endpoint generates a user nkey per identity and gets a scoped
    `orchestrator` user JWT for it from the control plane
    (`POST /v1/credentials/nats`), the same shape as a device in §3.
- **`stub_control_plane/`** is a **DEV ONLY** authorization server, to be
  replaced by the private control plane with the same HTTP API. It has:
  - RFC 8414 metadata, DCR, CIMD;
  - PKCE S256, auto-approved consent, refresh rotation with reuse detection;
  - a redirect allowlist with the claude.ai and ChatGPT callbacks;
  - `claude-web-<n>`-style identities, one per connector install;
  - NATS user JWTs signed with the tenant's scoped role key (Python `nkeys`).
- **Tests.** The e2e test runs on the H-0 operator stack with two endpoint
  replicas and the MCP SDK's OAuth client: DCR → authorize → token, then
  delegate/result, inbox, paging, sessions and a wave across separate HTTP
  sessions. `scripts/dev/remote_mcp_demo.sh` prints the URL for claude.ai's
  "Add custom connector" when `cloudflared` is available. A live claude.ai
  run was not possible in the build environment.
- **Gaps** (listed in `REMOTE_MCP.md`): no login/consent UI or KMS, no SSRF
  guard on CIMD fetches, ChatGPT callback shapes not checked live, no MCP
  Events push.

## 5. Per-tenant hub-server

- **One container per tenant**, running
  `hub-server --durable --require-bound-identity --api-admin <owner identity>`
  with embedded SurrealDB on a persistent volume. This is exactly the
  self-hosted reference deployment, so hosted and self-host don't drift.
- **Why not one multi-tenant hub-server?** hub-server and its embedded DB are
  single-tenant by design (one RocksDB, one writer). One container per tenant
  is simple, isolated and fine for the first hundreds of tenants. Revisit if
  per-container overhead dominates cost.
- **Durability makes restarts cheap.** With `--durable`, sends wait in the
  tenant's `HUB_INGRESS` while its hub-server restarts or redeploys, and route
  when it's back. This also makes **scale-to-zero** possible later (wake on
  pending ingress). Not v1.
- **HA:** a single replica with auto-restart. RPO comes from volume snapshots
  (W2-B backup). True HA with one consistent history needs SurrealDB server
  mode behind the `Storage` trait (`DELIVERY.md` → "Several hub-servers"),
  which is future work.
- **hub-server changes needed:** almost none for v1. It already runs inside
  one account without knowing about tenants. Needed: readiness/health
  endpoint for the orchestrator (container platform), tenant label on
  `/metrics`, and the WS bridge/visualizer authenticated by an ACS session
  instead of a shared bridge token (dashboard, H-4).

## 6. Control plane (private repo)

Not published (license section). Owns:

- users, orgs/tenants, devices, identities; OAuth sign-in, device-code flow,
  service keys, and the OAuth authorization server the remote MCP endpoint
  (§4b) uses
- account JWT creation and push to the resolver; account signing keys in KMS
- hub-server provisioning (create volume + container, rolling upgrades)
- usage metering (from account JetStream usage and hub-server metrics), billing
  (Stripe), audit log

Storage: managed Postgres. The control plane needs boring relational data and
backups, unlike the per-tenant message history. Language: open (Rust/axum
shares the JWT/nkey code with `acs`; TypeScript is faster for the web app).

## 7. Hosted ops

- **NATS:** 3-node cluster, JetStream R3 for tenant streams (`replicas` is
  already in `--delivery-config`), TLS terminated at nats-server
  (`wss://connect.acs…`, ACME certs). One region first.
- **Limits per tenant** (account JWT): connections, subscriptions, payload
  size, JetStream storage/streams/consumers. Keep the `HUB_*` stream limits
  under the account quota (`DELIVERY.md` → hosted notes).
- **Encryption at rest:** encrypted disks, plus JetStream `cipher` for stored
  DMs and task traffic (closes the "plaintext on the nats-server host"
  residual risk for hosted).
- **Monitoring:** nats-server `/varz`/`/jsz`, hub-server `/metrics` per
  tenant, alerts on routing lag (`HUB_INGRESS` pending), hub-server restarts
  and quota near limits.
- **Backups:** SurrealDB volume snapshots (W2-B); JetStream is a buffer, not
  the archive.
- **Secrets:** operator key offline; system account creds only on ops hosts;
  account signing keys in KMS; hub-server creds per tenant, injected at
  deploy.

## 8. Security, hosted view

The residual risks in `SECURITY.md` change meaning once a tenant is a team:

- **Workload channels are team-public:** still true, but only *within the
  tenant*. Acceptable for v1 (a tenant is one team). Per-task confidentiality
  stays a later feature.
- **The hub-server credential is all-powerful:** bounded to one tenant's
  account.
- **A leaked credential is an identity takeover:** bounded by 24 h JWT expiry
  and revocation.
- **New risk: cross-tenant isolation.** NATS accounts do the heavy lifting.
  We still need an isolation test (two accounts, every subject family, JS API,
  `_INBOX`, `$SYS`) in CI. Part of H-0.
- **New risk: the control plane** holds account signing keys. KMS, audit log,
  admin MFA.

## 9. Self-host stays first-class

- Same `hub-server`, same clients. Config-mode auth (`hub-admin
  render-config`) stays the default for self-hosters.
- Operator mode becomes an *option* for self-hosters too (we'll document what
  the spike builds).
- `acs` gets a `--server` flag to point at a self-hosted control plane only
  if we ever open one. Otherwise self-hosters use creds files as today
  (`JOIN_HUB.md`).

## 10. Phases

| Phase | What | Repo | Exit criteria |
|---|---|---|---|
| **H-0 spike** ✅ | Operator spike implemented: see §3 H-0 results and `OPERATOR_MODE.md` (remote-MCP research tracked separately as H-0b). Operator mode + resolver; scoped signing keys reproducing `acl.rs` per role; run `dogfood_prod_config.sh` and `dogfood_durable.sh` against an operator-mode account; two-tenant isolation test; JWT expiry/refresh/revocation with our clients; decide on extra channels; write up callout vs JWT; check remote-MCP connector support (OAuth, tool timeouts, plan/approval requirements) in claude.ai, ChatGPT and Grok | public (test harness + docs) | Both dogfoods pass in operator mode; isolation test green; decisions recorded here |
| **H-1 connector** ✅ (H-1a crate split + H-1c `acs`, 2026-10-21; `docs/ACS_CONNECTOR.md`) | Client crate split; `acs` login (incl. headless/service key)/up/status/agent/mcp install against a stub control plane; creds refresh; packaging (brew, curl install) | public | `acs login` → `acs mcp install claude` → delegate a task on a local operator-mode stack; a bot host joins headless |
| **H-1b remote MCP endpoint** ✅ (2026-10-21; `docs/REMOTE_MCP.md`; not yet tried live in claude.ai/ChatGPT) | Streamable-HTTP MCP server backed by JetStream (stateless), OAuth against the stub control plane, turn-bounded waits | public | claude.ai (as a custom connector) delegates to a local worker and fetches the result in a later turn |
| **H-2 control plane MVP** | Sign-up, tenant create (account JWT push + hub-server container), device-code flow, identity minting, revocation | private | A new user goes from sign-up to a delegated task with no manual steps |
| **H-3 hosted ops + private beta** | Cluster, TLS, limits, encryption at rest, monitoring, backups (W2-B) | private infra | 2 weeks of beta tenants without manual intervention |
| **H-4 product** | Dashboard (visualizer behind ACS login, W2-C), billing, self-serve | private + public | Paid sign-up works |

W2-B (retention, backup) feeds H-3; W2-C (visualizer) becomes the H-4
dashboard; W2-D (file locks) is independent. All three are done (2026-10-21).

**Status (2026-10-21):** everything that can be built in the public repo
without cloud resources is done and runs end to end locally
(`scripts/dogfood_hosted.sh`, transcript in
`.planning/hosted/live-e2e-2026-10-21.md`). Next: try the remote MCP
endpoint as a real claude.ai/ChatGPT custom connector through a tunnel, then
H-2 (private control plane) and H-3 (cloud), which need a private repo and
infrastructure decisions (§11).

## 11. Open decisions

The full list, with options and recommendations, is in
[`OPEN_DECISIONS.md`](OPEN_DECISIONS.md). This section keeps the original numbering.

**Beta decisions (2026-10-22, details in `OPEN_DECISIONS.md`)** override
§6–7 for the beta:
- **Control plane:** Rust/axum, in the private repo `agentmemorylabs/acs-private`.
- **Sign-in:** GitHub + Google OAuth + email magic links, with invite codes.
- **Tenancy and domain:** one user = one tenant; domain `acs.agentmemorylabs.com`.
- **Infra:** one GCP us-east VM running everything as containers. That
  means 1 NATS node, self-run Postgres and file-based signing keys
  (3 nodes, managed Postgres and KMS come later).
- **Beta:** free, invite-only.

1. **License split:** Apache-2.0 client + BUSL-1.1 server (Additional Use
   Grant: no third-party hosted service; Change Date 4 years → Apache-2.0) +
   private control plane. Agreed; licensor "Agent Memory Labs". **Applied**
   2026-10-20: see [`LICENSING.md`](../LICENSING.md).
2. **Extra broadcast channels** under scoped signing keys (H-0).
3. **Cloud and region** for the hosted cluster.
4. **Control-plane language** (Rust vs TypeScript).
5. **Identity naming:** free-form per tenant (current rules), or
   `<device>-<agent>` by default.
6. **MCP-only install without `acs`** (after beta).
7. **Pricing unit:** per seat, per agent identity, or usage (messages/storage).
