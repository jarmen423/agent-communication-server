# Execution roadmap — distributed hub readiness

**Status: COMPLETE** (waves 1–3 landed on `main`, July 2026)

| Commit | Wave | What |
|--------|------|------|
| (wave-1 era) | remote connectivity | ACP HTTP, OpenCode/Kilo ACP, NATS WS TLS/auth **config** examples |
| `cce1b7a` | wave-2a | Python `nats_connect` + Rust `HubConnectOptions` + systemd/SECURITY/JOIN/OPERATOR docs |
| `357f8f3` | wave-2b | Remote install DX, token/WS dogfood, Discord bridge |
| `3849bdf` | wave-3 | Full-stack env auth (hub-server/ApiClient), `list_pending` fix, `dogfood_wss_tls.sh` |

## Product outcome (done)

One central hub (`nats-server` + `hub-server`); remote machines dial in as **clients**
with token/TLS (`remote_agent_adapter`, bridges, CLIs via `NATS_*` env).

**Prove anytime:**

```bash
bash scripts/dogfood_token_auth.sh   # ws:// + token
bash scripts/dogfood_wss_tls.sh      # wss:// + CA + full-stack token
```

**Living docs (prefer these over handoffs):**

| Doc | Role |
|-----|------|
| [`docs/SECURITY.md`](../../docs/SECURITY.md) | Auth model |
| [`docs/OPERATOR_HUB.md`](../../docs/OPERATOR_HUB.md) | Stand up hub |
| [`docs/JOIN_HUB.md`](../../docs/JOIN_HUB.md) | Join hub |
| [`docs/REMOTE_INSTALL.md`](../../docs/REMOTE_INSTALL.md) | Thin remote package |
| [`docs/REMOTE_AGENTS.md`](../../docs/REMOTE_AGENTS.md) | Adapter + flags |
| [`docs/BRIDGES.md`](../../docs/BRIDGES.md) | Telegram + Discord |
| [`README.md`](../../README.md) | Distributed hub index |

Handoffs under `handoffs/` are **historical evidence** (what each leaf shipped).  
If a handoff conflicts with living docs, **living docs win**.

## Explicitly still out of scope (not wave 1–3)

| Item | Where |
|------|--------|
| **`hub-tui`** (ratatui daily driver) | [`docs/TUI_PLAN.md`](../../docs/TUI_PLAN.md) — plan only |
| Real public VPS cutover (Let’s Encrypt, secrets manager) | Operator runbook; not automated here |
| SMS/Slack/Email bridges | Copy Discord/Telegram pattern in `docs/BRIDGES.md` |
| Per-agent NATS users in dogfood (uses shared token) | Prod conf example supports users |

## Historical task tables

Preserved in git history and per-task handoffs. `tasks.json` is marked all-completed.
