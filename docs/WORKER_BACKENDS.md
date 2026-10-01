# Worker backend types

All workers use **`worker_runtime.run_worker()`** (NATS inbox, hub-delegate, hub-session, hub-wave). Backends only implement `run(prompt, ctx) -> (text, ctx)`.

## Types

| Type | Module | Mechanism |
|------|----------|-----------|
| **HeadlessCli** | `worker_backends/headless_cli.py` | Subprocess per turn, prompt on argv; supports resume/continue and streaming (stream-json / JSONL) parsers |
| **SdkAgent** | `worker_backends/sdk_agent.py` | In-process SDK; blocking calls run in thread pool |
| **AcpAgent** | `worker_backends/acp_agent.py`, `acp_stdio.py`, `acp_http_backend.py` | Long-lived ACP agent over JSON-RPC (stdio: hermes, grok, opencode; HTTP: kilo) |
| **$ExecCli** | one-off Rust `hub-worker --execute` | Stdin prompt; one-shot unless the command is session-aware |

The Claude Code and Codex workers are HeadlessCli subclasses
(`worker_backends/claude_code.py`, `worker_backends/codex_cli.py`); see
[Claude Code](#claude-code) and [Codex](#codex) below.

## Process safety (all subprocess backends)

Shared plumbing lives in `worker_backends/proc.py`:

| Guarantee | Detail |
|---|---|
| Timeout | Every HeadlessCli turn has a limit (default **900 s**; `--timeout-secs` on the claude/codex/grok workers). |
| No orphans | The CLI runs in its own process group. On timeout or cancellation the whole group gets SIGTERM, then SIGKILL, and is reaped before the error is raised. |
| Worker shutdown | Worker entrypoints call `install_worker_signal_handlers()`: SIGTERM/SIGHUP is forwarded to every live CLI group, and an `atexit` hook SIGKILLs anything left. (The supervisor's `killpg` of the worker does not reach the CLI's separate group on its own.) |
| Exit codes | A non-zero exit is always an error, with a stderr tail in the message, even if stdout was produced. |
| Pipes | stderr is drained concurrently into a bounded tail buffer. stdout is read in chunks (no 64 KiB line limit) and streamed line by line to the backend's parser. |
| `--` | `HeadlessCliSpec(end_of_options=True)` puts `--` before a positional prompt, so a prompt that starts with `-` is never parsed as a flag (claude, codex, kilo, opencode). |

ACP stdio backends (`worker_backends/acp_stdio.py`, used by `hermes_acp.py`, `grok_acp.py` and `opencode_acp.py`; no extra Python package needed):

- stderr is drained into a bounded buffer and quoted in errors when the agent dies.
- The client advertises **no** `fs`/`terminal` capabilities, and any agent→client request it doesn't implement gets JSON-RPC `-32601`.
- `session/request_permission` is answered by picking from the offered `options` **by kind**, per `permission_policy` (`allow_once` | `allow_always` | `reject`; `--permission-policy` on the ACP workers, default `allow_always`). If nothing matches, the answer is `cancelled`.
- When stdout hits EOF, the backend is marked dead and pending requests fail with the stderr tail. The next turn restarts the agent and opens a fresh session, because sessions from the dead process are not reused.

## Cancel (refocus-iteration-2.md §4.2)

A DM with `kind = control` and payload `{"action": "cancel", "task_id": <task envelope id>}`
stops that task. The worker publishes exactly one terminal result on the task
channel with `status: "cancelled"` (payload `{status, task_id, result: null,
error: "cancelled"}`), a `status: cancelled` envelope, and an `error` event
with `cancelled: true` (so wave watchers see a terminal task). Unknown or
finished task ids are ignored, with a debug log and no reply.

The runtime's inbox callback never blocks: envelopes are queued and run one at
a time (`worker_backends/inbox.py`), and a cancel is applied immediately. A
queued delegated task is answered `cancelled` at once and never runs.

| Backend type | What cancel does |
|---|---|
| HeadlessCli (claude, codex, agy, kilo, opencode, grok -p, hermes chat) | SIGTERM, then SIGKILL, of the CLI's process group, which is reaped before the result is published |
| ACP stdio (hermes, grok, opencode acp) | ACP `session/cancel` notification, then a wait of up to `cancel_grace_sec` (5 s) for the prompt to end. An agent that ignores it has its process group killed, and the next turn restarts it. |
| ACP HTTP (kilo acp) | ACP `session/cancel` notification (the agent is remote; there is no local process) |
| SdkAgent (cursor, echo) | A Python thread can't be killed: the turn is abandoned and its eventual result discarded. Pass `cancel_sync=` when the SDK has a stop call. |
| Rust `hub-worker --execute` | SIGKILL of the command's process group |

`hub-delegate`: the first Ctrl-C sends the cancel and waits up to 10 s for the
`cancelled` result (exit code 4). A second Ctrl-C exits at once (exit code 130).

## Progress handlers are per turn

Each turn runs `backend.run` in its own asyncio task with that turn's progress
handler in a context variable (`worker_backends/progress.py`). Backends read it
with `current_progress_handler()` at the start of `run()`. ACP backends capture
it while they hold their turn lock, because their stream arrives on a reader
task. Two concurrent session turns on one worker therefore never publish each
other's progress. `set_progress_handler()` still exists as a fallback for direct
callers and tests.

## Session resume (stub until T2)

`worker_backends/session_resume.py`, off unless `NATS_HUB_SESSION_RESUME=1`:
on startup the worker calls `hub.api` `session.list {worker, status: "active"}`,
resubscribes to each session channel, and hydrates `backend_ctx` from
`session.get` (`data.session.backend_ctx`). A `session_send` for an unknown
session does the same. After every successful turn the worker saves
`backend_ctx` with `session.update_backend_ctx {session_id, backend_ctx}`.
That op name is the integration point to confirm against T2.

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
- `hermes_spec()` → `hermes chat -Q`, `--resume`
- `grok_spec()` → `grok -p` headless (prefer ACP for multi-turn)
- `kilo_spec()` → `kilo run --format json --auto` (positional prompt, `--session` resume)
- `opencode_spec()` → `opencode run` (positional prompt, `--session` resume)

## Starting a worker

There is one entrypoint per worker type. Each accepts
`--identity --repo --nats-url [--model]` (`--nats-url` defaults to `$NATS_URL`)
and reads NATS auth from the `NATS_*` env vars (`nats_connect.py`). The
supervisor's table, `worker_backends/providers.py`, is the source of truth.
`tests/python/test_worker_entrypoints.py` starts every listed type with the
supervisor's argv against fake CLIs and requires a reply-contract answer.

| Provider id | Entrypoint | Needs |
|---|---|---|
| `claude` | `claude_worker.py` | `claude` CLI |
| `codex` | `codex_worker.py` | `codex` CLI |
| `grok` | `grok_acp_worker.py` | `grok` (`$GROK_BIN`) |
| `hermes` | `hermes_acp_worker.py` | `hermes` (`$HERMES_BIN`) |
| `agy` | `agy_worker.py` | `agy` |
| `cursor` | `cursor_worker.py` | `CURSOR_API_KEY` + `make setup-extras` (cursor-sdk) |
| `kilo` | `kilo_worker.py` | `kilo` |
| `kilo-acp` | `kilo_acp_worker.py` | a running `kilo acp --port` server (`--port` / `$KILO_ACP_PORT`) |
| `opencode` | `opencode_worker.py` | `opencode` |
| `opencode-acp` | `opencode_acp_worker.py` | `opencode` (`$OPENCODE_BIN`) |
| `echo` | `echo_worker.py` | nothing (testing) |

Not spawned by the supervisor, but still usable by hand: `grok_worker.py`
(headless `grok -p`) and `hermes_worker.py` (`hermes chat -Q`).

```bash
.venv/bin/python claude_worker.py --identity claude-1 --repo /path/to/repo
.venv/bin/python codex_worker.py  --identity codex-1  --repo /path/to/repo
.venv/bin/python hermes_acp_worker.py --identity hermes-1 --repo /path/to/repo

# Remote agent over WebSocket (distributed teams)
python3 remote_agent_adapter.py \
    --identity remote-1 \
    --nats-url ws://hub-host:8080 \
    --backend shell --execute "my-agent"
```

Workers subscribe to `channel.inbox.<identity>` and stay alive for sessions and wave tasks.

## Model catalog (all providers)

Model dropdowns in the visualizer are **not hard-coded**. They load live via:

```
browser  →  WS list_models {provider}
         →  hub-server
         →  hub.worker.models
         →  worker_supervisor
         →  worker_backends/model_catalog.py
         →  that provider's CLI / static source / config override
```

| Provider | Model source today |
|----------|--------------------|
| `kilo`, `kilo-acp` | `kilo models` |
| `opencode`, `opencode-acp` | `opencode models` |
| `cursor` | `agent models` (or `cursor-agent models`) |
| `agy` | `agy models` |
| `claude` | static list of `claude --model` aliases (`opus`, `sonnet`, `haiku`, `fable`); full names via **Other…** |
| `codex` | `codex debug models` (JSON; hidden entries skipped) |
| `hermes`, `grok` | no stable list yet → empty + **Other…** |
| `echo` | static empty (ignores models) |

### Add / configure a future provider

Edit `config/provider_models.json` (merged over built-ins):

```json
{
  "providers": {
    "my-agent": {
      "kind": "cli",
      "cmd": ["my-agent", "models"],
      "parser": "plain_ids",
      "label": "My Agent"
    }
  }
}
```

`kind` options:
- `cli` — run a command; parsers: `plain_ids`, `id_dash_label`, `plain_lines`
  (plus `codex_models_json` for the `codex debug models` catalog)
- `static` — embed `models: [{value,label}, ...]`
- `none` — empty list with a `reason` string for the UI

Also register a spawn entry in `worker_supervisor.py::PROVIDER_CMDS` so the
visualizer can actually launch that provider.

CLI check:

```bash
# direct
python3 -c "import asyncio; from worker_backends.model_catalog import list_models; \
  print(asyncio.run(list_models('kilo')))"

# via supervisor
# request-reply on hub.worker.models {"provider":"kilo"}
```

## Agent setup guide

Operator reference for configuring each supported agent CLI. Covers model
strings, auth requirements, and CLI-specific quirks discovered during
integration testing.

### Claude Code

**Binary:** `claude` (Claude Code CLI, checked against v2.1.x). Authenticate once with `claude auth` (or `ANTHROPIC_API_KEY`).

**Worker:** `claude_worker.py` → `worker_backends/claude_code.py`. Each turn runs:

```
claude -p --output-format stream-json --verbose --permission-mode <mode> \
       [--model M] [--allowed-tools=T1,T2] [--resume <session_id>] -- <prompt>
```

with cwd = `--repo`. The `session_id` comes from the stream (`system/init` or
`result`) and is stored in the hub-session ctx (`claude_session_id`), so later
turns of a `hub-session` continue the same Claude conversation. One-shot
delegations always start fresh.

Stream-json handling: `assistant` text blocks become `progress` events
(`phase: message`), `thinking` becomes `phase: thinking`, and `tool_use`
becomes `phase: tool`. The task result is the `result` event's text.
`is_error: true` or an `error_*` subtype fails the task, as does a non-zero exit.

| Flag | Default | Notes |
|---|---|---|
| `--repo` | cwd | working directory for `claude` |
| `--model` | CLI default | alias (`opus`, `sonnet`, `haiku`, `fable`) or full model name |
| `--permission-mode` | `acceptEdits` | `acceptEdits`, `auto`, `manual`, `dontAsk`, `plan` |
| `--dangerously-skip-permissions` | off | the **only** way to get `bypassPermissions`; sandboxes only |
| `--allowed-tools` | none | comma list, e.g. `"Read,Edit,Bash(git *)"` (passed as `--allowed-tools=…` because the flag is variadic) |
| `--timeout-secs` | 900 | per turn; the claude process group is killed on timeout |
| `--claude-bin` | `$CLAUDE_BIN` or `claude` | |

**Safety defaults:** `acceptEdits` auto-approves file edits in `--repo`, but
not arbitrary shell commands. The worker runs headless with no permission-prompt
host, so tool calls that would need approval are not auto-approved. Grant them
explicitly with `--allowed-tools`. `bypassPermissions` via `--permission-mode`
is refused.

```bash
.venv/bin/python claude_worker.py --identity claude-1 --repo ~/code/project --model sonnet
```

### Codex

**Binary:** `codex` (Codex CLI, checked against v0.159). Log in with `codex login`.

**Worker:** `codex_worker.py` → `worker_backends/codex_cli.py`. Turns run:

```
first turn:   codex exec --json -C <repo> -s <sandbox> [-m M] -o <tmpfile> -- <prompt>
session turn: codex exec resume --json -c sandbox_mode="<sandbox>" [-m M] -o <tmpfile> -- <thread_id> <prompt>
```

`codex exec resume` accepts neither `-C` nor `-s`, so session turns run with cwd
= `--repo` and set the sandbox through a config override. The `thread_id` comes
from `thread.started` and is stored as `codex_thread_id`.

JSONL handling: `reasoning` items become `phase: thinking`, while
`command_execution`, `file_change`, `mcp_tool_call` and `web_search` become
`phase: tool`, and a completed `agent_message` becomes `phase: message`. The
result is the last `agent_message`, with the `-o` last-message file as a
fallback. `turn.failed`, a non-zero exit, or no final message fails the task.

| Flag | Default | Notes |
|---|---|---|
| `--repo` | cwd | Codex working root |
| `--model` | CLI default | a slug from `codex debug models` |
| `--sandbox` / `-s` | `workspace-write` | `read-only`, `workspace-write`, `danger-full-access` |
| `--skip-git-repo-check` | off | needed when `--repo` is not a git repo |
| `--dangerously-bypass-approvals-and-sandbox` | off | the **only** way to drop sandbox + approvals; externally sandboxed hosts only |
| `--timeout-secs` | 900 | per turn; the codex process group is killed on timeout |
| `--codex-bin` | `$CODEX_BIN` or `codex` | |

```bash
.venv/bin/python codex_worker.py --identity codex-1 --repo ~/code/project
```

### Supervisor (`worker_supervisor.py`)

`hub.worker.ensure {identity, provider, model?}` spawns the worker for a
provider (`claude` → `claude_worker.py`, `codex` → `codex_worker.py`, …; there
is no silent echo fallback).

- **Logs:** each child's stdout+stderr is appended to `.tools/run/workers/<identity>.log` (`--log-dir`). A failed ensure includes a log tail.
- **Ready:** when the runtime logs its inbox subscription, or on the first `hub.presence` heartbeat, whichever comes first. Heartbeats only start after 30 s. A worker that exits during start is reported as `ok: false`.
- **Restarts:** a crashed child restarts with exponential backoff (1 s, 2 s, 4 s, … ≤ 30 s), at most `--max-restarts` (default 5) per 5 minutes, then it is abandoned (logged).
- **Shutdown:** SIGINT/SIGTERM/SIGHUP SIGTERM every child's process group, then SIGKILL after 5 s. `hub.worker.stop` does the same for one child.

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

**Requires:** the Python `cursor-sdk` package (`make setup-extras`) and `CURSOR_API_KEY`.

**Worker example:**
```bash
python3 cursor_worker.py --identity cursor-worker-1 --repo /path/to/repo
```

### Echo (testing)

Built-in echo backend for infrastructure testing — no model calls.

### JS workers (removed)

`worker.js` and `hub_worker.js` (Node + `@cline/sdk` 0.0.x) were removed in
iteration 2. Only `--type cline` ever worked; `agy`, `hermes` and `cursor`
threw "Unsupported type". They broke the reply contract: they sent two
results (task channel plus a DM), DM'd status, read the task channel from
`meta.reply_to`, never registered, had no auth, and couldn't cancel. They had
no tests and the supervisor never spawned them. Every type they advertised has
a Python worker above. A Cline worker, if wanted, belongs as a HeadlessCli
preset over the `cline` CLI.

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
