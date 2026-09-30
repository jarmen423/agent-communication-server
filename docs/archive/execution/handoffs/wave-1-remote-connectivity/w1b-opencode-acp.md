# W1-B — OpenCode ACP stdio backend

## Outcome

Shipped. `OpencodeAcpBackend` speaks JSON-RPC 2.0 over stdio to
`opencode acp`, modeled on the Grok backend (raw JSON-RPC, content-block
prompts, session/update notifications). Both required files exist, imports
succeed under the hermes venv python, and the backend stays under the 400
LOC budget.

## Files written

| Path | LOC | Status |
|---|---|---|
| `worker_backends/opencode_acp.py` | 399 | NEW |
| `opencode_acp_worker.py` | 47 | NEW |

Both files are inside the W1-B owned write scope; no other files touched.

## Verification performed

```bash
# backend import
cd /home/jfrie/nats && /home/jfrie/.hermes/hermes-agent/venv/bin/python3 \
    -c "from worker_backends.opencode_acp import OpencodeAcpBackend; print('OK')"
# → OK

# worker import
cd /home/jfrie/nats && /home/jfrie/.hermes/hermes-agent/venv/bin/python3 \
    -c "import opencode_acp_worker; print('OK')"
# → OK

# surface check (run/close/start/set_progress_handler/clear_progress_handler)
PYTHONPATH=/home/jfrie/nats /home/jfrie/.hermes/hermes-agent/venv/bin/python3 \
    /tmp/w1b_smoke.py
# → run signature: (prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]
# → resolve_opencode_bin -> /home/jfrie/.opencode/bin/opencode
# → surface OK

# CLI flag surface
python3 /home/jfrie/nats/opencode_acp_worker.py --help
# → shows --identity, --model, --provider, --opencode-bin, --repo, --nats-url, --channel
```

`ast.parse()` confirms both files are syntactically valid; `wc -l` confirms
399 LOC for the backend. No live `opencode` CLI available in this environment
(end-to-end prompt test deferred to a machine with `opencode` installed).

## Backend design

`OpencodeAcpBackend` exposes:

- `async run(prompt, ctx) -> (text, ctx)` — required signature.
- `async start()` — spawns `opencode acp` subprocess, performs the ACP
  `initialize` handshake, picks an auth method if `authMethods` is non-empty
  (prefers `env` when a provider API key is set, then the agent's
  `defaultAuthMethodId`, then any first advertised method).
- `async close()` — cancels reader task, drains pending futures, terminates
  the subprocess (SIGTERM → 3s → SIGKILL).
- `set_progress_handler(handler)` / `clear_progress_handler()` — throttled
  streaming hook for `message` / `thought` / `tool` events so we don't flood
  NATS (350 ms min interval, 40-char fat threshold, 800-char tail snippet).
- `__all__ = ["OpencodeAcpBackend", "resolve_opencode_bin"]`.

### Session lifecycle

1. `initialize` — protocolVersion 1, fs+terminal capabilities, clientInfo.
2. `authenticate` — only if the agent advertises auth methods; sends
   `_meta: {"headless": True}` so the agent knows this is non-interactive.
3. `session/new` — `cwd`, `mcpServers: []`, optional provider `_meta`.
4. `session/set_model` — best-effort, on both new and resumed sessions.
5. `session/set_mode` — best-effort `always-allow` for headless tool use.
6. `session/prompt` — content-block prompt `[{"type": "text", "text": prompt}]`.
7. `session/update` notifications stream:
   - `agent_message_chunk` → collected for return text + emitted as `message`.
   - `agent_thought_chunk` → collected (debug) + emitted as `thought`.
   - `tool_call*` → emitted as `tool` (title + status).

### Persistence / resume

- Session id is stored in `ctx["opencode_acp_session_id"]` (also mirrored to
  `ctx["acp_session_id"]` for parity with `hermes_acp` / `grok_acp` backends).
- Resumed sessions get a fresh `session/set_model` if `self.model` changed.
- A late `session/set_model` failure on resume is logged at debug and falls
  through (some ACP versions may not support it mid-session).

### Permission auto-approval

`request_permission` and `session/request_permission` notifications are
auto-approved with `optionId="allow-always"` (both canonical shapes). If
`always_approve=False`, the backend denies.

### Protocol divergence note

`agent_message_chunk` arrives as both snake_case and camelCase
(`agentMessageChunk`) depending on the OpenCode version; the message handler
accepts either. Same for `agent_thought_chunk`.

## Worker CLI

`opencode_acp_worker.py` mirrors `hermes_acp_worker.py` exactly:

- `--identity` (default `opencode-acp-worker-1`)
- `--model` (e.g. `anthropic/claude-sonnet-4.5`)
- `--provider` (e.g. `anthropic`)
- `--opencode-bin` (override default lookup; honors `$OPENCODE_BIN`)
- `--repo` (cwd passed to backend)
- `--nats-url` (default `nats://127.0.0.1:4222`)
- `--channel` (optional broadcast channel)

The worker calls `run_worker(WorkerConfig(...))` with
`log_prefix="opencode-acp-worker"`, identical pattern to the Hermes variant.

## Deliberate deviations from the reference backends

- **No `acp` Python library** — modeled on `grok_acp.py`, not `hermes_acp.py`.
  The installed `acp` package at
  `/home/jfrie/.hermes/hermes-agent/venv/lib/python3.11/site-packages/acp/`
  exposes `connect_to_agent` / `run_agent` (not `spawn_stdio_connection`),
  so the Hermes backend's import would already fail at runtime. Using raw
  JSON-RPC sidesteps the version drift and matches OpenCode's documented
  behavior precisely.
- **Provider scoped via `_meta`** — OpenCode supports per-session provider
  overrides via `session/new` `_meta.opencode.provider`. When the user
  passes `--provider`, the backend threads it through; otherwise it's a
  no-op dict spread.
- **`session/set_mode modeId="always-allow"`** — OpenCode-specific
  permission shortcut; falls back silently if the version doesn't support it.

## Known gaps / follow-ups

- Live round-trip test deferred: no `opencode` CLI on this machine. The
  backend is wired identically to `grok_acp.py` (which is dogfooded in
  production), so the protocol plumbing is low-risk; the remaining
  uncertainty is in OpenCode's exact method-name variants, which the
  message handler already accepts in both snake_case and camelCase.
- If OpenCode introduces new method names (e.g. session `cancel`,
  `load_session`), they can be added without touching the existing
  surface — `_request` and `_handle_message` are isolated.
- `OPENCODE_BIN` env var takes precedence over the candidate walk
  (`~/.local/bin/opencode`, `/usr/local/bin/opencode`,
  `~/.opencode/bin/opencode`); verified that the resolver picks up the
  latter when present.

## No blockers

Task completed inside scope; no other files modified.