# H-0 — Hosted spike: operator mode, scoped signing keys, tenant isolation

Read `_COMMON.md` first. Then read `docs/HOSTED_PLAN.md` (all of it, but §3 is
the design you are proving), `docs/SECURITY.md`, `docs/DELIVERY.md` and
`src/bin/hub_admin/acl.rs`.

Branch: `iter2/h0-operator`. Worktree: `/data/code/acs-wt/h0-operator`.

## Why

The hosted product puts each tenant in its own NATS **account** in operator
(JWT) mode. Agents get user JWTs signed by **scoped signing keys** whose
permission templates use `{{name()}}`. Before anything is built on that
design, prove that our *unchanged* hub-server, Rust clients, Python workers and
MCP server work under it, that tenants are isolated, and that short-lived
credentials rotate without breaking clients.

## Acceptance criteria

1. **One source of truth for permissions.**
   - `hub-admin` can emit the per-role permission templates (worker,
     orchestrator, admin, service, hub-server) with `{{name()}}` in place of
     the identity. They come from the same code as `acl.rs`'s `user_entry`
     (refactor so both call one function).
   - A unit test expands each template for sample identities and asserts the
     result equals today's config-mode allow lists exactly.
2. **Operator-mode stack.**
   - New `scripts/dev/operator_stack.sh` builds a throwaway operator-mode
     setup and exports what tests need. The setup is an operator, a system
     account, two tenant accounts `T_A`/`T_B` with JetStream limits, scoped
     signing keys per role, users per identity, and nats-server with the
     account resolver.
   - Use `nsc` (pin and checksum it the way `scripts/dev/install_nats_server.sh`
     pins nats-server, installed into `.tools/bin`) or mint the JWTs
     programmatically. Record which, and what the control plane would use in
     production.
3. **Our dogfoods pass in operator mode.**
   - `dogfood_prod_config.sh` and `dogfood_durable.sh` run against tenant
     `T_A` and pass. Parametrize them (e.g. `--operator`) instead of forking
     them.
   - hub-server runs with `--durable --require-bound-identity` as tenant
     A's hub-server user.
4. **Tenant isolation**, a Rust or Python test plus a dogfood section. An agent
   in `T_B` cannot:
   - see or publish on any of `T_A`'s `hub.*`, `channel.*`, `_INBOX.*` or
     `hub.delivery.*`;
   - use its `$JS.API.*`, or reach `$SYS`;
   - reach `T_A`'s hub-server query API.

   Both tenants run their own hub-server at the same time with the same
   subject and stream names, and each routes only its own traffic.
5. **Short-lived credentials.**
   - Users with a short expiry (e.g. 60 s). When the creds file is rewritten
     with a fresh JWT, Rust `HubClient`, nats-py workers and the MCP server
     reconnect with the new one.
   - If a client reads the creds file only once, fix it within write scope
     (async-nats has auth callbacks; nats-py has signature/JWT callbacks).
   - A revoked user (account revocation list) is disconnected and can't
     reconnect.
6. **Decisions recorded** in a new "H-0 results" subsection of
   `docs/HOSTED_PLAN.md` §3:
   - per-agent extra broadcast channels (tenant-wide channels in the
     template, or unscoped keys with explicit permissions);
   - operator/JWT versus auth callout, with real observations;
   - whatever surprised you.

   Add a self-host guide, new `docs/OPERATOR_MODE.md`.

## Write scope

Owns:
- `src/bin/hub_admin.rs`, `src/bin/hub_admin/**`
- `src/connect_opts.rs`, `nats_connect.py`, `mcp_server/hub_connection.py`: credential reload only. Run `scripts/dev/sync_plugins.sh` afterwards.
- `scripts/dogfood_prod_config.sh`, `scripts/dogfood_durable.sh`: parametrize
- new `scripts/dev/operator_stack.sh`, new `scripts/dev/install_nsc.sh`
- new `tests/operator_*.rs`, `tests/python/test_operator_*.py`. Skip cleanly when the tools aren't installed.
- `config/*operator*` examples
- `docs/HOSTED_PLAN.md` (§3 results, §10 H-0 row), new `docs/OPERATOR_MODE.md`
- `.github/workflows/ci.yml`: add a step that installs the pinned tool and runs the operator tests

Must NOT edit:
- router, query API, orchestrator, storage, analytics, `src/delivery/**`, `hub_server.rs`. If operator mode needs a change there, stop and report it.
- `Cargo.toml` and `Makefile`: propose any new dependency or target instead.
- The W2-B task runs in parallel and owns storage, analytics and `hub_server.rs`.

## Done means

- All six criteria are met with tests that run in CI.
- `make lint` and `make test` are green.
- The operator-mode dogfoods are green, with their real output in the report.
- The branch is pushed and its CI is green.
