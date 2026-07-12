# Visualizer Operator Guide

The nats-hub visualizer is a browser-based real-time dashboard for agent
activity. It streams all bus envelopes over a WebSocket bridge and lets you
send messages, start/stop agents, and watch live progress events.

## Architecture

```
[Browser]  ←─ WebSocket (ws://) ─→  [hub-server WS bridge]  ←─ NATS ─→  [workers, agents]
                                         ↓
                                   Static file server (visualizer HTML/JS)
```

The WS bridge (`src/ws_bridge.rs`) is embedded in `hub-server`. When you pass
`--ws-addr`, hub-server opens a TCP listener that:

1. Serves static files (the visualizer HTML/JS/CSS) from `--static-dir`
2. Accepts WebSocket upgrades on `/ws` and streams all `channel.>` envelopes
3. Accepts JSON commands from the browser and publishes them onto NATS

## Prerequisites

1. **NATS server** running with the nats-hub config:
   ```bash
   nats-server -c config/nats-server.conf
   ```

2. **hub-server binary** built:
   ```bash
   CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats cargo build
   ```

3. (Optional) **Worker supervisor** for on-demand agent spawning via the UI:
   ```bash
   /home/jfrie/.hermes/hermes-agent/venv/bin/python3 worker_supervisor.py
   ```

## Starting the visualizer

```bash
# Start hub-server with the visualizer
cd ~/nats  # ensure working directory is the repo root
/data/cargo-targets/jfrie/nats/debug/hub-server \
    --db-path /tmp/nats_hub.db \
    --ws-addr 127.0.0.1:9191 \
    --static-dir /home/jfrie/nats/visualizer/
```

Then open **http://127.0.0.1:9191/** in your browser.

Flags:
- `--ws-addr` — address for the WebSocket bridge + static file server (default: disabled)
- `--static-dir` — directory to serve static files from (usually `visualizer/`)
- `--db-path` — SurrealDB path for message persistence (required for history features)
- `--metrics-addr` — optional Prometheus metrics endpoint (e.g. `127.0.0.1:9090`)

## Full stack startup

From the nats-hub repo root, in separate terminals:

```bash
# Terminal 1: NATS server
nats-server -c config/nats-server.conf

# Terminal 2: hub-server (router + DB + visualizer)
cd ~/nats
/data/cargo-targets/jfrie/nats/debug/hub-server \
    --db-path /tmp/nats_hub.db \
    --ws-addr 127.0.0.1:9191 \
    --static-dir /home/jfrie/nats/visualizer/

# Terminal 3: worker supervisor (on-demand agent spawning)
/home/jfrie/.hermes/hermes-agent/venv/bin/python3 worker_supervisor.py

# Terminal 4: a worker (or let the supervisor spawn it from the UI)
python3 kilo_worker.py --identity kilo-worker-1 --model kilo/minimax/minimax-m3
```

Then open http://127.0.0.1:9191/ in your browser.

## Visualizer capabilities

### Viewing

- **Live message feed** — all `channel.>` envelopes stream in real time
- **Agent presence** — agents appear as animated sprites (from Petdex) or
  octagonal chips when no sprite is available
- **Status indicators** — working (green), idle (ready), error (red), closed (gray)
- **Progress events** — streaming thought/message/tool updates appear as they
  arrive from ACP-backed workers

### Browser commands

The visualizer sends JSON commands over the WebSocket connection. These are
handled by `handle_client_command()` in `ws_bridge.rs`:

| Command | Action |
|---------|--------|
| `{"type":"send_message","to":"agent","message":"..."}` | Ensure worker exists, then delegate task |
| `{"type":"send_message","to":"agent","message":"...","provider":"kilo","model":"kilo/minimax/minimax-m3"}` | Spawn worker with provider+model if needed, then delegate |
| `{"type":"ensure_worker","identity":"...","provider":"...","model":"..."}` | Spawn a worker without sending a task (model optional) |
| `{"type":"stop_agent","identity":"..."}` | Stop a supervised worker and mark closed |
| `{"type":"resume_agent","identity":"..."}` | Mark agent as ready again |

The `provider` field maps to entries in `worker_supervisor.py::PROVIDER_CMDS`.
Supported values: `grok`, `hermes`, `echo`, `agy`, `cursor`, `kilo`, `kilo-acp`,
`opencode`, `opencode-acp`, `codex`, `claude`.

The `model` field is optional. When provided, the supervisor passes `--model`
to the worker process. If a worker for that identity is already running with a
different model, the supervisor restarts it.

### Pets (agent sprites)

Agent sprites come from Petdex installs under `visualizer/pets/<slug>/`. Each
pet has a `spritesheet.webp` (192×208 cells, 6 frames per animation row). If no
sprite loads, the visualizer falls back to an octagonal chip with a glyph.

See `docs/pets.md` for sprite format details.

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| Blank page at :9191 | `--static-dir` not set or wrong path | Pass `--static-dir visualizer/` relative to repo root |
| WebSocket connects but no messages | hub-server can't reach NATS | Verify NATS is running: `ss -tlnp \| grep 4222` |
| Messages appear but agents don't respond | No workers running, supervisor not running | Start a worker or the supervisor |
| `ensure_worker` returns error | Unknown provider or worker binary not found | Check `PROVIDER_CMDS` in `worker_supervisor.py` |
| Agent appears but never goes "ready" | Worker process crashed during startup | Check worker stdout/stderr for auth or model errors |
| Visualizer shows stale agents | Presence heartbeats stopped | Workers heartbeat every 30s; restart crashed workers |

## Remote access

The visualizer binds to `127.0.0.1` by default. For remote access:

- **SSH tunnel:** `ssh -L 9191:127.0.0.1:9191 user@host`
- **Bind to all interfaces:** `--ws-addr 0.0.0.0:9191` (ensure the port is
  firewalled — the visualizer has no built-in auth)

For a production deployment, put the visualizer behind a reverse proxy with
authentication (nginx, Caddy) rather than exposing the port directly.
