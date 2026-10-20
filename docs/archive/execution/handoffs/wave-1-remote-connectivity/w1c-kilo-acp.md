# W1-C — Kilo ACP HTTP backend

## Outcome

Shipped. `KiloAcpBackend` speaks JSON-RPC 2.0 over streamable HTTP to a
remote `kilo acp --port` server, layered on top of the W1-A
`AcpHttpTransport` / `AcpHttpBackend`. Both required files exist, imports
succeed under the hermes venv python, and the backend lands at 200 LOC — well
inside the 400 LOC budget.

## Files written

| Path | LOC | Status |
|---|---|---|
| `worker_backends/kilo_acp.py` | 200 | NEW |
| `kilo_acp_worker.py` | 65 | NEW |

Both files are inside the W1-C owned write scope. No other files were
modified; `acp_http.py` (W1-A) was read only, never edited.

## Verification performed

```bash
# backend import
cd /home/jfrie/nats && /home/jfrie/.hermes/hermes-agent/venv/bin/python3 \
    -c "from worker_backends.kilo_acp import KiloAcpBackend; print('OK')"
# → OK

# worker import
cd /home/jfrie/nats && /home/jfrie/.hermes/hermes-agent/venv/bin/python3 \
    -c "import kilo_acp_worker; print('OK')"
# → OK

# CLI surface
python3 /home/jfrie/nats/kilo_acp_worker.py --help
# → --identity, --model, --port, --hostname, --repo, --nats-url, --channel

# Surface + URL resolver + auth-header graceful fallback
PYTHONPATH=/home/jfrie/nats /home/jfrie/.hermes/hermes-agent/venv/bin/python3 \
    /tmp/w1c_smoke.py
# → __all__ = ['KiloAcpBackend', 'resolve_kilo_url']
# → resolve_kilo_url()                    → http://127.0.0.1:8721/acp
# → resolve_kilo_url(9000)                → http://127.0.0.1:9000/acp
# → resolve_kilo_url(9000, 'kilo.int')    → http://kilo.int:9000/acp
# → resolve_kilo_url(9000, 'h', scheme='https') → https://h:9000/acp
# → unauthenticated KiloAcpBackend        → auth_headers={}
# → with KILO_API_KEY=sk-test-123         → auth_headers={'Authorization': 'Bearer sk-test-123'}
# → has run, close, start, set_progress_handler, clear_progress_handler: OK
```

`ast.parse()` confirms both files are syntactically valid; `wc -l` confirms
200 LOC for the backend. No live `kilo` CLI available in this environment
(end-to-end prompt test deferred to a machine with `kilo` installed).

## Backend design

`KiloAcpBackend` is a thin specialization over `AcpHttpBackend`. Everything
protocol-shaped — JSON-RPC correlation, SSE stream handling, header
management (`Acp-Connection-Id` / `Acp-Session-Id`), `initialize` /
`session/new` / `session/prompt` lifecycle, agent_message_chunk
collection, progress throttling — lives in `acp_http.py`. The Kilo
backend adds only the Kilo-specific concerns:

- **`resolve_kilo_url(port, hostname, *, scheme, path)`** — pure helper
  that builds `http://<hostname>:<port>/acp`. Defaults to `127.0.0.1:8721`
  (the standard Kilo ACP port).
- **`_resolve_kilo_auth_headers()`** — reads `KILO_API_KEY` /
  `KILO_AUTH_TOKEN` from the env and produces a `Bearer` header.
  Unauthenticated start is a no-op: an empty dict keeps the worker
  running, and the server surfaces 401/403 if it actually needs auth.
- **Session namespace** — persists the Kilo session id under both
  `ctx["kilo_acp_session_id"]` (canonical) and `ctx["acp_session_id"]`
  (legacy parity with the hermes/grok/opencode backends).
- **`session/set_model` after `session/new`** — best-effort. Some Kilo
  versions accept the model only via this follow-up call rather than
  inline on `session/new` (which `AcpHttpBackend` already attempts).
  Failure here is logged at debug and falls through.
- **Progress handler pass-through** — `set_progress_handler` /
  `clear_progress_handler` proxy to the inner backend, which already
  throttles `message`/`thought`/`tool` events at 350 ms / 40 chars
  (the same shape as `grok_acp.py`).
- **`close()`** — calls the inner backend's `close()`, which cancels
  SSE tasks, drains pending futures, and closes the HTTP client.
- **`__all__ = ["KiloAcpBackend", "resolve_kilo_url"]`**.

### Lifecycle (delegated to `AcpHttpBackend`)

1. `initialize` — once per worker, sets up `Acp-Connection-Id`.
2. Open the connection-scoped SSE stream.
3. `session/new` — `cwd`, optional `model`. Captures `sessionId`.
4. Open the per-session SSE stream.
5. `session/prompt` — `content-block` prompt `[{type:"text", text:prompt}]`.
6. `agent_message_chunk` notifications accumulate → returned text.
7. Best-effort `session/set_model` (Kilo-specific retry).
8. Next turn: reuses `ctx["kilo_acp_session_id"]`, skips steps 3–4.

### Resume / multi-turn

`hub-session` reuses the same ctx, so the second `run()` call jumps
straight to `session/prompt` against the existing Kilo session. The
namespacing under `kilo_acp_session_id` keeps this backend's state
isolated from any other ACP backend's session id.

### Permission auto-approval

Delegated to `AcpHttpBackend`: `request_permission` and
`session/request_permission` are auto-acked with
`{outcome: {outcome: "selected", optionId: "allow-always"}}`. With
`always_approve=False` the backend denies.

## Worker CLI

`kilo_acp_worker.py` mirrors `hermes_acp_worker.py` exactly, plus the
remote-connection flags specific to this backend:

- `--identity` (default `kilo-acp-worker-1`)
- `--model` (e.g. `anthropic/claude-sonnet-4`)
- `--port` (default `8721`, honors `KILO_ACP_PORT` env)
- `--hostname` (default `127.0.0.1`)
- `--repo` (cwd passed to backend)
- `--nats-url` (default `nats://127.0.0.1:4222`)
- `--channel` (optional broadcast channel)

The worker calls `run_worker(WorkerConfig(...))` with
`log_prefix="kilo-acp-worker"`. Operationally the worker assumes the
HTTP server is started out-of-band (`kilo acp --port 8721 --cwd <dir> &`),
and this script just dials in.

## Deliberate deviations from the reference backends

- **`AcpHttpBackend` owns lifecycle, not us.** Both `grok_acp.py` and
  `opencode_acp.py` reimplement the JSON-RPC reader loop and subprocess
  plumbing. The HTTP transport already exposes a complete
  `WorkerBackend.run()` shape, so re-implementing it would duplicate work.
  `KiloAcpBackend` is ~200 LOC instead of ~400.
- **No subprocess.** Unlike `grok` (stdio) or `opencode` (stdio), `kilo acp
  --port` is a long-lived HTTP daemon started out-of-band. We only model
  *connection*, not *process*.
- **Auth header is graceful.** The Grok backend raises when no auth method
  is available; here we just don't send a header and let the server
  decide. This keeps an unauthenticated dev server usable without env
  setup.
- **`session/set_model` post-hoc.** `grok_acp.py` does this same retry on
  `session/new`; Kilo is the same shape, so we apply the same retry.

## Known gaps / follow-ups

- Live round-trip test deferred: no `kilo` CLI on this machine. The
  backend reuses `AcpHttpBackend` (which itself is well-isolated), so
  the protocol plumbing risk is low.
- The `kilo auth login` flow currently maps only to `KILO_API_KEY` /
  `KILO_AUTH_TOKEN` env vars. If Kilo exposes a richer credential cache
  (e.g. via a socket or credential file), `resolve_kilo_auth_headers()`
  is the single place to add it.
- HTTP/2 is on by default (mirroring `AcpHttpBackend`). If a particular
  Kilo version rejects h2 the worker can pass `--http2=...` if added
  later; surface is read-only on `self.http2`.

## No blockers

Task completed inside scope; no other files modified.
