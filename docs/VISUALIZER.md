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

Set up the repo first; see [`CONTRIBUTING.md`](../CONTRIBUTING.md) (`make setup && make build`).

## Quickest path

```bash
make up    # nats-server + hub-server (visualizer on :9191) + echo workers
```

Then open **http://127.0.0.1:9191/** in your browser.

## Starting the visualizer manually

From the repo root (paths are relative; `--static-dir` must point at `visualizer/`):

```bash
.tools/bin/nats-server -c config/nats-server.conf        # or: nats-server -p 4222 -js

./target/debug/hub-server \
    --db-path .tools/run/nats_hub.db \
    --ws-addr 127.0.0.1:9191 \
    --static-dir "$PWD/visualizer/"
```

Flags:
- `--ws-addr`: address for the WebSocket bridge and static file server (default: disabled)
- `--static-dir`: directory to serve static files from (usually `visualizer/`)
- `--db-path`: SurrealDB path for message persistence (needed for history features)
- `--metrics-addr`: optional Prometheus metrics endpoint (e.g. `127.0.0.1:9090`)

If you've set `CARGO_TARGET_DIR`, binaries are under `$CARGO_TARGET_DIR/debug/` instead of `./target/debug/`.

## Full stack startup

From the repo root, in separate terminals:

```bash
# Terminal 1: NATS server
.tools/bin/nats-server -c config/nats-server.conf

# Terminal 2: hub-server (router + DB + visualizer)
./target/debug/hub-server \
    --db-path .tools/run/nats_hub.db \
    --ws-addr 127.0.0.1:9191 \
    --static-dir "$PWD/visualizer/"

# Terminal 3: worker supervisor (spawns agents on demand from the UI)
.venv/bin/python worker_supervisor.py

# Terminal 4: a worker (or let the supervisor spawn one from the UI)
.venv/bin/python kilo_worker.py --identity kilo-worker-1 --model kilo/minimax/minimax-m3
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

Model **dropdown choices** are loaded live per provider:

```json
{"type":"list_models","provider":"kilo"}
{"type":"list_providers"}
```

These hit `hub.worker.models` / `hub.worker.providers` on the supervisor and use
`worker_backends/model_catalog.py` so every supported (and future configured)
provider can supply its own list — not a hard-coded kilo/opencode table.

### Pets (agent sprites)

Agent sprites come from Petdex installs under `visualizer/pets/<slug>/`. Each
pet has a `spritesheet.webp` (192×208 cells, 6 frames per animation row). If no
sprite loads, the visualizer falls back to an octagonal chip with a glyph.

See `docs/pets.md` for sprite format details.

## Scene interaction

### Move agents

| Gesture | Result |
|---------|--------|
| **Click** (press + quick release, no hold/drag) | Action menu (Open chat / Stop / Resume) |
| **Click-and-hold** (~220ms) | Grab agent; move follows pointer; **no menu** on release |
| **Click + drag** past a few pixels | Same as hold — reposition, **no menu** |

Pinned positions are stored in `localStorage` (`nats-hub.positions`) as normalized
coords and restored on reload / resize. **Reflow** in the HUD SCENE tools unpins
everyone and restores the automatic ring layout. **Unstick** ejects any agent that
landed under the HUD / AGENTS dock / chat panel (those overlays steal mouse events,
so a sprite buried under them cannot be grabbed). Drag clamp + load/resize rescue
also prevent re-parking under chrome.

### Custom background

No stock catalog yet. Operators upload their own floor image:

1. HUD → **SCENE** → **Upload BG**
2. Pick any image file (jpeg/png/webp/…)
3. Image is compressed (max edge 1920, JPEG) and drawn cover-fit under the grid
4. Persisted as `localStorage` key `nats-hub.scene-bg` when quota allows
5. **Clear BG** removes the custom image

Grid + a light dim overlay stay on top so sprites remain readable.

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
