# Worker backend types

All workers use **`worker_runtime.run_worker()`** (NATS inbox, hub-delegate, hub-session). Backends only implement `run(prompt, ctx) -> (text, ctx)`.

## Types

| Type | Module | Mechanism |
|------|----------|-----------|
| **HeadlessCli** | `worker_backends/headless_cli.py` | Subprocess, prompt on argv/stdin; supports resume/continue |
| **SdkAgent** | `worker_backends/sdk_agent.py` | In-process SDK; blocking calls run in thread pool |
| **AcpAgent** | `worker_backends/acp_agent.py` | Protocol transport when stdio/HTTP/WebSocket ACP is available |
| **$ExecCli**** | one-off Rust `hub-worker --execute` | Stdin prompt; one-shot unless the command is session-aware |

## Presets

See `worker_backends/presets.py`:
- `agy_spec()` → `--continue` per session cwd
- `hermes_chat_q_spec()` → `--resume`
- `cursor_sdk_spec()` → Cursor SDK resume via `agent_id`