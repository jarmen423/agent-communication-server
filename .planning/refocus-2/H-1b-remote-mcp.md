# H-1b — Hosted remote MCP endpoint (for web chat apps) + dev stub control plane

Read `_COMMON.md` first. Then read `docs/HOSTED_PLAN.md` §1 ("Kinds of
agents"), §3, §4b and §6, `.planning/hosted/H-0b-web-mcp.md` (the per-platform
facts), `docs/OPERATOR_MODE.md`, `docs/DELIVERY.md`, and the current
`mcp_server/` (canonical stdio MCP server).

Branch: `iter2/h1b-remote-mcp`. Worktree: `/data/code/acs-wt/h1b-remote-mcp`.

## Goal

Web chat apps (claude.ai, ChatGPT, Grok) add ACS as a custom **remote MCP
connector**: streamable HTTP plus OAuth. Each connector install becomes one
identity in the user's tenant. Build that endpoint, and a **dev-only stub
control plane** that issues the OAuth tokens and the NATS credentials it
needs. Everything is Python and Apache-2.0 (client side). Reuse the existing
`mcp_server/` tool logic rather than forking it.

## Acceptance criteria

1. **`mcp_remote/` serves MCP over streamable HTTP** (the official `mcp`
   Python SDK's streamable-HTTP server or equivalent).
   - It exposes the hub tool set: delegate, inbox, task results/wait,
     cancel, waves and sessions where sensible.
   - The tool logic is shared with `mcp_server/`. Refactor `mcp_server/`
     handlers so the transport is pluggable. The stdio server and the plugin
     copies (run `scripts/dev/sync_plugins.sh`) must keep working, and the
     existing MCP tests stay green.
2. **It holds no state between requests.**
   - Inbox reads and task results come from JetStream: the identity's durable
     `inbox-<id>` consumer (attach on `hub.delivery.<id>.inbox`) and
     `HUB_TASKS` direct gets.
   - Nothing lives only in process memory, so two endpoint replicas behind a
     load balancer give the same answers. Test this with two instances.
   - It requires a `--durable` hub.
3. **Turn-bounded tools.**
   - `delegate` returns a `task_id` within a few seconds and never waits for
     the worker.
   - `get_task_result(task_id, wait_secs ≤ 50)` returns the terminal result
     or "still running". Default the wait cap to under 60 s (ChatGPT's
     reported limit).
   - Long results are paged (cursor), each page ≤ 100k characters.
   - Every read-only tool has `readOnlyHint: true`, and every tool has a
     `title` and `destructiveHint` where it applies (Claude directory
     requirement).
4. **OAuth per the MCP authorization spec.**
   - The endpoint is the resource server:
     - It answers unauthenticated calls with `401` +
       `WWW-Authenticate: Bearer resource_metadata=…`.
     - It serves RFC 9728 protected-resource metadata.
     - It validates access tokens issued by the stub. Choose JWT or
       introspection, and document the choice.
   - The token maps to `(tenant, identity)`. The endpoint connects to NATS as
     that identity with its own user credentials (operator mode, scoped
     keys). Identity always comes from the token, never from tool arguments.
5. **`stub_control_plane/` (dev only, clearly labelled not-for-production)
   is the OAuth authorization server.**
   - It serves RFC 8414 metadata, dynamic client registration (RFC 7591),
     client ID metadata documents (CIMD) if feasible, the authorization-code
     flow with PKCE S256 (consent auto-approves a fixed dev user), token and
     refresh with rotation, and an allowlist of redirect URIs, including
     `https://claude.ai/api/mcp/auth_callback` and ChatGPT's connector
     callback.
   - On consent it mints an identity for the connector install, e.g.
     `claude-web-<n>`. It creates a NATS user in the tenant account with the
     right scoped role key. Reuse `scripts/dev/operator_stack.sh` / `nsc` from
     H-0, or mint JWTs with the Python `nkeys` library.
   - Design its HTTP API so the `acs` connector (H-1c, next wave) can add
     the device-code flow and per-device identity minting to it later.
6. **Tests and a demo.**
   - An automated end-to-end test runs on the operator stack from H-0:
     - stub control plane;
     - durable, bound hub-server in tenant `T_A`;
     - an echo worker;
     - the remote MCP endpoint ×2;
     - an MCP SDK client doing the full OAuth dance (DCR → authorize → token)
       and then `delegate` → `get_task_result` across **separate** HTTP
       sessions, which simulates separate chat turns.
   - Add a `scripts/dev/remote_mcp_demo.sh` that brings the stack up and
     prints the URL to paste into claude.ai's "Add custom connector" when a
     tunnel (e.g. `cloudflared`) is available. Don't require a tunnel in tests.
   - A live claude.ai test is not possible in this environment. Say so.
7. **Docs.** New `docs/REMOTE_MCP.md` covers the architecture, the OAuth
   flow, the tool list with hints, limits and how to run the demo. Update
   `docs/HOSTED_PLAN.md` §4b with what was built.

## Write scope

Owns:
- new `mcp_remote/**`, new `stub_control_plane/**`
- `mcp_server/**`: the refactor for a shared transport. Then run `sync_plugins.sh` (the plugin copies follow).
- new `tests/python/test_remote_mcp*.py`, `tests/python/test_stub_cp*.py`
- `scripts/dev/remote_mcp_demo.sh`, `requirements*.txt` (new Python deps, pinned)
- new `docs/REMOTE_MCP.md`, `docs/HOSTED_PLAN.md` §4b
- `.github/workflows/ci.yml`: a step for your tests. Keep the edit small, because H-1a edits CI too.

Must NOT edit:
- any Rust: `src/**`, `Cargo.*`. H-1a is splitting the crate in parallel. If you need a Rust change (e.g. hub-server behaviour), stop and report it.
- `REUSE.toml`/`LICENSING.md`: Python under new top-level dirs defaults to Apache-2.0 already; confirm with the licensing test.
- `Makefile`: propose targets instead.

## Done means

- All seven criteria are met.
- `make lint` and `make test` are green; the operator suite and your new e2e test are green.
- The branch is pushed and its CI is green.
- The report gives the OAuth flow you implemented step by step, plus any spec gaps against claude.ai/ChatGPT requirements.
