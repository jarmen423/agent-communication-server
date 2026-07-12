# Worker backend types

All workers use **`worker_runtime.run_worker()`** (NATS inbox, hub-delegate, hub-session, hub-wave). Backends only implement `run(prompt, ctx) -> (text, ctx)`.

## Types

| Type | Module | Mechanism |
|------|----------|-----------|
| **HeadlessCli** | `worker_backends/headless_cli.py` | Subprocess, prompt on argv/stdin; supports resume/continue |
| **SdkAgent** | `worker_backends/sdk_agent.py` | In-process SDK; blocking calls run in thread pool |
| **AcpAgent** | `worker_backends/acp_agent.py` | Protocol transport when stdio/HTTP/WebSocket ACP is available |
| **$ExecCli** | one-off Rust `hub-worker --execute` | Stdin prompt; one-shot unless the command is session-aware |

## Runtime modes

`worker_runtime.py` handles all inbox traffic on a single worker process:

| Mode | Trigger | Channel |
|------|---------|---------|
| **One-shot** | `hub-delegate` DM (no `action`) | `task.<uuid>` |
| **Session** | `action: session_start` on inbox | `session.<uuid>` (or custom `channel`) |
| **Wave task** | `action: session_start` + `wave_id` | `wave.<id>.task.<task_id>` + wave broadcast |

## Progress events

`worker_events.py` wraps each turn with structured events (`MessageKind::Event`):

| Event type | When |
|---|---|
| `started` | Before model/subprocess execution |
| `progress` | Mid-execution updates |
| `completed` | Task done (result in `data.result`) |
| `error` | Task failed |
| `milestone` | Significant step (e.g. `verify_passed` after `verify_cmd`) |

Observe in real time: `hub-watch --session <id>` or `hub-watch --wave <id>`.

Wave tasks mirror events to both the task channel and `wave.<id>` so the orchestrator and dependents see completion.

## Presets

See `worker_backends/presets.py`:
- `agy_spec()` → `--continue` per session cwd
- `hermes_chat_q_spec()` → `--resume`
- `cursor_sdk_spec()` → Cursor SDK resume via `agent_id`
- `grok_spec()` → `grok -p` headless (prefer ACP for multi-turn)
- `kilo_spec()` → `kilo run --format json --auto` (positional prompt, `--session` resume)
- `opencode_spec()` → `opencode run` (positional prompt, `--session` resume)

## Starting a worker

```bash
# Python (any backend)
python3 cursor_worker.py --identity cursor-worker-1 --repo /path/to/repo
python3 hermes_acp_worker.py --identity hermes-worker-1
python3 grok_worker.py --identity grok-worker-1          # headless -p
python3 grok_acp_worker.py --identity grok-acp-1         # ACP stdio sessions
python3 kilo_worker.py --identity kilo-worker-1           # Kilo CLI
python3 opencode_worker.py --identity opencode-worker-1   # OpenCode CLI

# Universal JS entrypoint
node hub_worker.js --type cursor --identity cursor-worker-1

# Remote agent over WebSocket (distributed teams)
python3 remote_agent_adapter.py \
    --identity remote-1 \
    --nats-url ws://hub-host:8080 \
    --backend shell --execute "my-agent"
```

Workers subscribe to `channel.inbox.<identity>` and stay alive for sessions and wave tasks.

## Agent setup guide

Operator reference for configuring each supported agent CLI. Covers model
strings, auth requirements, and CLI-specific quirks discovered during
integration testing.

### Kilo

**Binary:** `kilo` (install via VS Code extension or `kilo upgrade`)

**Model strings:** Kilo uses its own prefix system — NOT standard `provider/model`. The format depends on which gateway/provider the model routes through:

| Gateway | Example model string | Notes |
|---------|---------------------|-------|
| Kilo Gateway | `kilo/minimax/minimax-m3` | Kilo's hosted gateway (OAuth). Requires credits at app.kilo.ai |
| OpenRouter | `openrouter/~openai/gpt-mini-latest` | Uses your OpenRouter API key |
| xAI | `grok-4.5` (bare, no prefix) | Requires xAI/Grok subscription |
| Z.AI | `z-ai/glm-4.6` (use `kilo/z-ai/glm-4.6` for kilo gateway) | Z.AI Coding Plan |

**Finding available models:** `kilo models` lists all models. Filter with `kilo models <gateway>` (e.g. `kilo models openrouter`). Model names are case-sensitive (`MiniMax-M3` ≠ `minimax-m3`) and the prefix format varies per gateway.

**Auth:** `kilo auth list` to see configured providers. Most providers need API keys or OAuth login (`kilo auth login`). The default gateway can run out of credits — check `kilo auth list` and your billing dashboard if you get 401/402 errors.

**Worker example:**
```bash
python3 kilo_worker.py --identity kilo-worker-1 --model kilo/minimax/minimax-m3
```

**Output parsing:** Kilo emits NDJSON event streams with `--format json`. The worker backend parses these automatically (`json_events=True` in the preset) and extracts text from `type: text` events. Session IDs are captured from the `sessionID` field for multi-turn resume.

### OpenCode

**Binary:** `opencode` (install from opencode.ai or `~/.opencode/bin/opencode`)

**Model strings:** OpenCode Zen models use the `opencode/` prefix:

| Gateway | Example model string | Notes |
|---------|---------------------|-------|
| OpenCode Zen | `opencode/deepseek-v4-flash-free` | Free tier available |
| OpenCode Zen | `opencode/claude-sonnet-4` | Requires credits at opencode.ai |

**Finding available models:** `opencode models` lists all models.

**Auth:** `opencode auth` to configure providers. The default OpenCode Zen gateway can run out of credits — check `opencode auth` and your workspace billing if you get 401 errors.

**Worker example:**
```bash
python3 opencode_worker.py --identity opencode-worker-1 --model opencode/deepseek-v4-flash-free
```

**Output parsing:** Same as Kilo — NDJSON event streams parsed automatically.

### Hermes

**Binary:** `hermes` (install via `pip install hermes-agent`)

**Model strings:** `provider/model` format (e.g. `anthropic/claude-sonnet-4`, `xai/grok-4.5`). Hermes resolves providers through its own config system.

**Auth:** Hermes uses its own provider config (`hermes config`). No separate gateway auth needed.

**Worker examples:**
```bash
# Headless CLI (one-shot per turn)
python3 hermes_worker.py --identity hermes-worker-1

# ACP backend (persistent session, streaming, tool progress)
python3 hermes_acp_worker.py --identity hermes-acp-1
```

### Grok

**Binary:** `grok` (install from x.ai)

**Auth:** `grok login` (OAuth) or `XAI_API_KEY` env var. Can hit spending limits on personal teams — error is `personal-team-blocked:spending-limit`.

**Worker examples:**
```bash
# Headless -p (one-shot)
python3 grok_worker.py --identity grok-worker-1 --model grok-4.5

# ACP stdio (persistent session, streaming)
python3 grok_acp_worker.py --identity grok-acp-1 --model grok-4.5
```

### Antigravity

**Binary:** `agy`

**Worker example:**
```bash
python3 agy_worker.py --identity agy-worker-1 --model <model>
```

### Cursor

**Requires:** Cursor SDK (`@cursor/sdk` npm package).

**Worker example:**
```bash
python3 cursor_worker.py --identity cursor-worker-1 --repo /path/to/repo
```

### Echo (testing)

Built-in echo backend for infrastructure testing — no model calls.

```bash
python3 echo_worker.py --identity echo-worker-1
```

### Troubleshooting model errors

| Error | Likely cause | Fix |
|-------|-------------|-----|
| `Insufficient balance` / `CreditsError` | Gateway credits exhausted | Check billing at the gateway's dashboard, or switch to a different provider model |
| `Model not found: X` | Wrong model string format | Check `kilo models` or `opencode models` for the exact name and prefix |
| `personal-team-blocked:spending-limit` | xAI spending limit hit | Add credits at grok.com or use a different provider |
| `UnknownError: Unexpected server error` | Often a follow-up to a model-not-found | Check the preceding error line for the real cause |
