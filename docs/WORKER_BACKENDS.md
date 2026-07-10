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

## Starting a worker

```bash
# Python (any backend)
python3 cursor_worker.py --identity cursor-worker-1 --repo /path/to/repo
python3 hermes_acp_worker.py --identity hermes-worker-1
python3 grok_worker.py --identity grok-worker-1          # headless -p
python3 grok_acp_worker.py --identity grok-acp-1         # ACP stdio sessions

# Universal JS entrypoint
node hub_worker.js --type cursor --identity cursor-worker-1
```

Workers subscribe to `channel.inbox.<identity>` and stay alive for sessions and wave tasks.
