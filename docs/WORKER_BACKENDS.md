# Worker backend types

All workers use **`worker_runtime.run_worker()`** (NATS inbox, hub-delegate, hub-session). Backends only implement `run(prompt, ctx) -> (text, ctx)`.

## Types

| Type | Module | When to use |
|------|--------|-------------|
| **HeadlessCli** | `worker_backends/headless_cli.py` | `agy -p`, `hermes chat -q`, any `CMD -flag PROMPT` |
| **SdkAgent** | `worker_backends/sdk_agent.py` | Cursor SDK, in-process APIs (sync fn in thread pool) |
| **AcpAgent** | `worker_backends/acp_agent.py` | Future ACP stdio/HTTP; stub + `AcpTransport` protocol |
| **StdinCli** | Rust `hub-worker` | `--execute` + prompt on stdin; one-shot unless command is session-aware |

**Node Cline** (`worker.js`) is still a separate **SdkAgent**-style process; can move to Python `SdkAgentBackend` or `AcpAgent` later.

## Headless CLI — one preset line

```python
from worker_backends.headless_cli import HeadlessCliBackend
from worker_backends.presets import agy_spec  # or hermes_spec, or HeadlessCliSpec(...)

await run_worker(WorkerConfig(
    identity="agy-worker-1",
    backend=HeadlessCliBackend(agy_spec(repo="/path", model="...")),
    log_prefix="agy-worker",
))
```

`HeadlessCliSpec` fields:

- `resume_mode`: `none` | `continue_flag` | `resume_id` | `session_cwd_continue`
- `session_cwd_subdir`: e.g. `.nats-hub/agy-sessions` (uses `ctx["_session_id"]` from runtime)
- `resume_ctx_key` / `has_turn_ctx_key`: where session state lives in `ctx`

Add a new CLI: copy `presets.py` pattern (~15 lines) + thin `*_worker.py` argparse.

## Sdk agent — one sync function

```python
backend = SdkAgentBackend(my_sync_run, log_label="cursor-worker")
```

## Acp agent — when protocol is wired

Implement `AcpTransport.send_turn(prompt, handle) -> (text, handle)` and pass to `AcpAgentBackend(transport)`.