# Remote MCP endpoint (web chat apps)

Web chat apps (claude.ai, ChatGPT, Grok) can't run a binary or keep a
connection open between turns. They add ACS as a **custom remote MCP
connector** instead: MCP over streamable HTTP, with OAuth sign-in. Each
connector install becomes one hub identity in the user's tenant, for example
`claude-web-1`. It can delegate to the tenant's agents, read its inbox and
fetch results in later turns. It acts only as an orchestrator, never as a
worker. See [`HOSTED_PLAN.md`](HOSTED_PLAN.md) §4b for the plan and the
per-platform facts.

Two pieces, both Python and Apache-2.0:

- **`mcp_remote/`** is the endpoint (the OAuth *resource server*). It is
  stateless, and you can run as many replicas as you like.
- **`stub_control_plane/`** is the OAuth *authorization server* and NATS
  credential issuer for dev and tests. **It is DEV ONLY, not for
  production.** It auto-approves a fixed dev user and reads tenant signing
  seeds from a local nsc store. The production control plane is private
  (`HOSTED_PLAN.md` §6) and implements the same HTTP API.

## Architecture

```
claude.ai / ChatGPT ──HTTPS: MCP (streamable HTTP) + Bearer token──▶ mcp_remote ×N
        │                                                              │  verify JWT (JWKS, offline)
        │ OAuth: register, authorize (browser), token, refresh         │  NATS as the token's identity
        ▼                                                              ▼
stub_control_plane ◀── POST /v1/credentials/nats (service key + token) ┘
   (authorization server, identity minting, user JWTs)
                                                   ┌──────── NATS, operator mode ────────┐
                                    mcp_remote ───▶│ account T_A: hub.pub.claude-web-1.… │◀── hub-server --durable
                                                   │ HUB_TASKS, HUB_INBOX/inbox-<id>     │    --require-bound-identity
                                                   └─────────────────────────────────────┘◀── workers
```

- **Identity.** It comes only from the verified access token's
  `acs_tenant` and `acs_identity` claims. Each tool call runs inside
  `hub_connection.bound(identity, …)`, so the shared handlers stamp and
  publish as that identity. A `from` argument that doesn't match the token is
  rejected. nats-server enforces the rest: the endpoint connects **as** the
  identity, with a user JWT signed by the tenant's scoped `orchestrator` key.
  That gives it the same permissions as any other orchestrator
  (`hub-admin permission-template --role orchestrator`).
- **NATS credentials.** Each replica generates its own user nkey for each
  identity it serves and keeps the seed in memory. It sends only the public
  key, with the caller's access token and the endpoint's service key, to
  `POST /v1/credentials/nats`. The control plane checks the token and the
  identity's status, then returns a user JWT (`name` = identity, 1 h by
  default). That is the device flow of `HOSTED_PLAN.md` §3 with the endpoint
  as the device. Seeds never leave the process, and signing keys never leave
  the control plane.
- **Statelessness.** The endpoint runs streamable HTTP in stateless mode, so
  there is no `Mcp-Session-Id` and any replica can answer any request.
  Nothing a tool answers depends on process memory:
  - task results and session replies are read back from the `HUB_TASKS`
    stream with `DIRECT.GET`;
  - the inbox is read from the identity's durable `inbox-<id>` consumer. Its
    ack floor is the read position, shared by all replicas;
  - the connection pool, the JWT cache and the JWKS are caches only.

  This needs `hub-server --durable`. On a core hub, `delegate`,
  `get_task_result`, `read_inbox` and sessions fail with "needs a durable
  hub".
- **Shared tool logic.** `mcp_server/` is still the canonical stdio server.
  `hub_app.build_server` builds the MCP server object for both transports.
  The remote endpoint reuses its schemas, annotations and handlers for agents,
  DMs, history, threads and waves. Only the tools that used to keep
  in-memory buffers (delegate, results, inbox, sessions) have stateless
  remote versions.

## OAuth flow

The endpoint follows the MCP authorization spec (2025-06-18 / 2025-11-25).
The MCP Python SDK's handlers enforce the protocol shape: PKCE `S256` only,
redirect URI equal to the registered one, client authentication, and code
expiry. The stub adds the policy.

1. The client calls `POST /mcp` without a token. The endpoint answers
   `401` with `WWW-Authenticate: Bearer error="invalid_token", …,
   resource_metadata="<base>/.well-known/oauth-protected-resource/mcp"`.
2. The client fetches that RFC 9728 document: `resource` = the endpoint URL,
   `authorization_servers` = [the control plane], `scopes_supported` =
   `["acs:hub"]`. The document is also served at the root well-known path.
3. The client fetches `/.well-known/oauth-authorization-server` (RFC 8414)
   from the control plane. It lists the authorize, token, register and
   revoke endpoints, `code_challenge_methods_supported: ["S256"]`,
   `client_id_metadata_document_supported: true`, the token auth methods
   `none`/`client_secret_post`/`client_secret_basic`, and `jwks_uri`.
4. **Client identity.** There are two ways:
   - **DCR** (RFC 7591): `POST /register`. Every redirect URI must be on the
     allowlist, otherwise the answer is `invalid_redirect_uri`.
   - **CIMD**: the client uses an https URL as its `client_id`. The stub
     fetches that document (5 s, 64 KiB, no redirects). It requires
     `client_id` to equal the URL, a public client (`none`), and allowlisted
     redirect URIs.

   The allowlist contains:
   - `https://claude.ai/api/mcp/auth_callback` and
     `https://claude.com/api/mcp/auth_callback`;
   - `https://chatgpt.com/connector_platform_oauth_redirect` and
     `https://chatgpt.com/connector/oauth/<one segment>`;
   - `--allow-redirect-uri` extras;
   - RFC 8252 loopback URIs, only with `--allow-loopback-redirects`.
5. `GET /authorize` with `response_type=code`, `code_challenge` +
   `S256`, `state`, `resource` (RFC 8707) and `scope=acs:hub`. The stub
   checks the redirect URI against the allowlist again, checks that
   `resource` is one of its `--resource` URLs, and redirects to
   `/consent?request=<id>`.
6. `GET /consent` auto-approves the fixed dev user (`dev-user`, tenant
   `--tenant`). It mints the install's identity the first time and reuses it
   while that install isn't revoked. The prefix comes from the redirect
   host: `claude-web-<n>`, `chatgpt-web-<n>`, `grok-web-<n>`, else
   `mcp-web-<n>`. The name is checked with `hub-admin`'s identity rules.
   Then it redirects to the client callback with a single-use code (5 min)
   bound to the client, redirect URI, PKCE challenge, scopes and resource.
7. `POST /token` (`authorization_code`). The SDK checks the code, its expiry,
   the redirect URI and `SHA256(code_verifier) == code_challenge`. The stub
   consumes the code and returns:
   - a 15-minute **access token**: an EdDSA JWT with `kid`, `iss`, `aud` =
     resource, `sub`, `client_id`, `scope`, `acs_tenant`, `acs_identity`,
     `jti`, `exp`;
   - a 30-day opaque **refresh token**, stored only as a SHA-256 hash.
8. The client retries `POST /mcp` with `Authorization: Bearer …`. The
   endpoint verifies the JWT against the JWKS (refetched at most every 30 s
   when it sees an unknown `kid`): signature, `iss`, `aud` equal to its own
   resource URL, `exp`, the `acs:hub` scope and a valid identity claim. Then
   it runs the call as that identity.
9. `POST /token` (`refresh_token`) **rotates** the refresh token: the new
   one replaces the old. Presenting a rotated-out token again revokes the
   whole grant (OAuth 2.1 reuse detection). `POST /revoke` revokes a
   refresh token's grant. A revoked access token can't get NATS credentials
   any more.

**Why JWT and not introspection.** Every tool call is verified offline, so
the control plane isn't on the hot path and replicas share only a public
key. The cost is that a revoked access token keeps working at the endpoint
until it expires (15 min). That is why access tokens are short-lived, and
why every refresh and every NATS credential issue rechecks the grant and the
identity.

## Tools

Every tool has a `title` and `readOnlyHint`/`destructiveHint`/
`idempotentHint`/`openWorldHint` annotations. Read-only tools run without a
confirmation prompt in ChatGPT, and Claude's directory requires the hints.

| Tool | Read-only | Destructive | What |
|---|---|---|---|
| `whoami` | ✔ | | identity and tenant from the token (no network) |
| `delegate` | | | DM a task to a worker; returns `task_id` and `worker` at once |
| `get_task_result` | ✔ | | terminal result, or `running`. `wait_secs` ≤ 50 (default 25); paged by `cursor` |
| `cancel_task` | | ✔ | §4.2 cancel DM; waits ≤ 20 s for `cancelled` |
| `read_inbox` | ✔ | | new DMs, oldest first; `wait_secs` ≤ 50. Each DM is returned once across turns and replicas |
| `start_session` / `send_to_session` | | | multi-turn session with a worker |
| `session_replies` | ✔ | | session channel after `since_seq` (a stream sequence); returns `next_seq` |
| `close_session` | | ✔ | `session_close` + record closed |
| `list_agents`, `get_agent` | ✔ | | shared |
| `send_message`, `send_direct` | | | shared |
| `create_wave`, `spawn_wave` | | | shared (hub-server drives the wave) |
| `cancel_wave` | | ✔ | shared |
| `wave_status`, `list_waves`, `get_wave`, `list_wave_tasks`, `get_wave_task` | ✔ | | shared |
| `list_sessions`, `get_session`, `get_history`, `get_thread`, `list_pending` | ✔ | | shared |

The stdio-only blocking tools (`delegate_task`, `wait_for_task`,
`wait_for_message`, `check_providers` pings) aren't offered: they wait
longer than a chat turn allows. The stdio server now carries the same
annotations.

`get_task_result` needs the `worker` that `delegate` returned. It is the C5
check (HARDENING.md): only that worker's correlated message counts as the
result. Another agent posting a "result" on the task channel is ignored.
Task, worker and session ids are pattern-checked, so an argument can't add
NATS subject tokens or wildcards.

## Limits

| Limit | Value | Why |
|---|---|---|
| Longest wait in one call | 50 s | ChatGPT's reported ~60 s tool-call limit; claude.ai allows 240 s |
| Result page | ≤ 100 000 characters, measured JSON-escaped | claude.ai caps a tool result at ~150 000 |
| Whole tool result | 120 000 characters (else an error asks for a smaller `limit`) | same |
| Prompt / message | 200 000 characters | |
| Inbox read | ≤ 50 DMs and ≤ 100 000 characters per call; DMs that don't fit are naked and come next time | |
| Retention | task/session channels 24 h, inbox 7 d | `DELIVERY.md` stream defaults |
| Access token | 15 min (`--access-ttl`); refresh 30 d; NATS user JWT 1 h (`--nats-ttl`) | |

An inbox DM is acked once it is in the tool result. If the endpoint dies
between fetch and ack, the DM is redelivered after the consumer's
`ack_wait` (30 s). A pull request that is still open when a call returns can
take a DM that then comes back after `ack_wait`. Either way the DM keeps its
`meta.id`.

## Run it

```bash
make setup && make build && scripts/dev/install_nsc.sh
scripts/dev/remote_mcp_demo.sh            # cloudflared on PATH → public https URLs
scripts/dev/remote_mcp_demo.sh --no-tunnel   # local only
```

The demo starts the operator stack (tenant `T_A`, durable bound hub-server),
an `echo-1` worker, the stub control plane and one endpoint, then prints the
**connector URL**. In claude.ai go to Customize → Connectors → "+" → Add
custom connector, paste the URL and press Connect. The consent step
auto-approves and the connector gets `claude-web-1`. With your own tunnel,
set `PUBLIC_ISSUER_URL` and `PUBLIC_MCP_URL` and forward them to
`CP_PORT`/`MCP_PORT`. Both URLs must be public: the browser opens
`/authorize`, and the chat app's servers call `/register`, `/token` and the
MCP URL.

By hand, against any operator-mode NATS:

```bash
.venv/bin/python -m stub_control_plane --port 9300 --resource http://127.0.0.1:9400/mcp \
    --nsc-home "$OPERATOR_DIR/nsc" --tenant T_A --service-key-file /tmp/acs/service.key
.venv/bin/python -m mcp_remote --port 9400 --resource-url http://127.0.0.1:9400/mcp \
    --issuer http://127.0.0.1:9300 --nats-url "$NATS_URL" --service-key-file /tmp/acs/service.key
```

The stub refuses to listen on a non-loopback address unless you pass
`--insecure-dev-public`. Behind a tunnel, set `--issuer` to the public URL;
the endpoint can still reach the control plane locally with `--cp-url`.
Neither process logs tokens, seeds or the service key. Access logs are off
unless you pass `--access-log`.

## Tests

- `tests/python/test_stub_cp.py`: metadata, the redirect allowlist, PKCE
  (missing or `plain` challenge refused, wrong verifier, code reuse),
  identity per install, refresh rotation and reuse detection, the NATS
  credential API (service key, forged token, revoked identity, JWT claims
  and signature), CIMD, the non-loopback guard. Runs in `make test`.
- `tests/python/test_remote_mcp.py`: `401` + `resource_metadata` for
  missing, foreign-key, wrong-audience, expired and wrong-scope tokens; the
  RFC 9728 document; tools/list and identity from the token; annotations;
  turn-bounded waits; subject-injection patterns; paging; the binding; the
  stream reader with forged results; the license map. Runs in `make test`.
- `tests/python/test_remote_mcp_e2e.py` runs on the operator stack. It
  starts the stub, a durable bound hub-server in `T_A`, `echo-1`, and two
  endpoint replicas behind a TCP proxy the test pins to either replica. An
  MCP SDK client does DCR → authorize → consent → token, then each "chat
  turn" is a new HTTP session:
  - `delegate` on replica 0, then `get_task_result` on replica 1 and 0;
  - a forged worker is ignored;
  - `from` can't override the identity, and DMs carry `meta.from` = the
    minted identity;
  - inbox DMs are read on alternating replicas, each exactly once;
  - a ~150k-character result is paged across replicas;
  - refresh rotation with reuse detection;
  - a session and a wave, each driven across both replicas.

  ```bash
  scripts/dev/operator_stack.sh .venv/bin/python -m pytest -q -s tests/python/test_remote_mcp_e2e.py
  ```

A live claude.ai or ChatGPT run was **not** possible in the build
environment (no public URL or browser account). The e2e test drives the same
protocol with the MCP SDK's own OAuth client.

## Known gaps

- **Not production.** The stub has no login, no consent UI, no Postgres,
  no KMS and no resolver push for revocations. A revoked identity's existing
  NATS JWT stays valid until it expires (≤ 1 h). The endpoint cuts the
  identity off within the access-token lifetime, because refresh and
  credential issue both stop.
- **CIMD fetching** is https-only, but private address ranges aren't
  blocked (SSRF). A production authorization server must block them and
  honour cache headers.
- **ChatGPT callbacks** come from H-0b research. The exact
  `connector/oauth/{callback_id}` shape and the stable redirect's host
  weren't confirmed against a live ChatGPT. Grok's OAuth requirements are
  undocumented.
- **No "lazy" or mixed auth.** Every call needs a token, and no tools are
  public. That is fine for claude.ai and for ChatGPT's "OAuth" mode.
- **No push.** ChatGPT's MCP Events (protocol `2026-07-28`) aren't
  implemented; the SDK negotiates up to `2025-11-25`. Results are polled.
- Results are JSON text content. There is no `structuredContent` or
  `outputSchema` yet.
- The authorization server's metadata `issuer` is normalised with a
  trailing slash (`https://cp.example/`), the same as the resource
  metadata's `authorization_servers`. The JWT `iss` claim has no slash.
  Clients don't read `iss`, and the endpoint checks it exactly.
- No per-identity rate limits yet.

## Extending it (H-1c)

`POST /v1/credentials/nats` is the hook for the `acs` connector:
- a device-code flow (RFC 8628, reserved paths `/device/code` and
  `/device/token`) issues tokens whose audience is the control plane itself;
- the same endpoint then signs per-device identities for a device-generated
  public key, without the service key;
- `Store.mint_identity(kind="device", …)` is the naming hook.
