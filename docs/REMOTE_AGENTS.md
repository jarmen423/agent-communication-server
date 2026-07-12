# Remote Agents

Remote agents connect to the nats-hub bus over **WebSocket** and participate as
full members — inbox routing, stateful sessions, waves, heartbeats — without
needing the NATS server binary, hub-server, or SurrealDB on the remote machine.

## Architecture

```
[Hub host]                              [Remote machine]
  NATS server (:4222 + :8080 WS)          agent CLI (kilo, opencode, etc.)
  hub-server (router)                       │
  SurrealDB (persistence)                   │
       ▲                                     │
       │ ws:// or wss://                     │
       └─────── remote_agent_adapter.py ─────┘
```

The remote machine needs only:

- Python 3.10+
- `nats-py` (`pip install nats-py`)
- The agent itself (CLI binary, ACP server, or SDK)

## Enable WebSocket on the hub

NATS server config (`config/nats-server.conf`):

```
websocket {
    port: 8080
    no_tls: true          # dev only; use cert_file/key_file for production
    same_origin: false
    handshake_timeout: "5s"
}
```

Start NATS with the config:

```bash
nats-server -c config/nats-server.conf
```

## Using the remote adapter

```bash
# Generic shell command backend
python3 remote_agent_adapter.py \
    --identity remote-worker-1 \
    --nats-url ws://hub-host:8080 \
    --backend shell \
    --execute "my-agent-cli --prompt"

# Kilo CLI
python3 remote_agent_adapter.py \
    --identity kilo-worker-1 \
    --nats-url ws://hub-host:8080 \
    --backend kilo \
    --model anthropic/claude-sonnet-4.5

# OpenCode CLI
python3 remote_agent_adapter.py \
    --identity opencode-worker-1 \
    --nats-url ws://hub-host:8080 \
    --backend opencode
```

The adapter uses `worker_runtime.run_worker()` — the same runtime as local
workers — so session/wave/event semantics are identical.

## Backends

| Backend | Flag | Mechanism |
|---------|------|-----------|
| `shell` | `--execute "cmd"` | Generic shell command, prompt as arg |
| `kilo` | `--model <model>` | `kilo run` headless CLI |
| `opencode` | `--model <model>` | `opencode run` headless CLI |

To add a new named backend, add a spec to `worker_backends/presets.py` and a
case in `remote_agent_adapter.py::build_backend()`.

## Security

For production:

1. **Enable TLS** on the WebSocket block (`cert_file`, `key_file`).
2. **Add NATS auth** — token, username/password, or NKeys:

```
authorization {
    users [
        { user: agent, password: secretpass,
          allowed_connection_types: ["WEBSOCKET"] }
    ]
}
```

3. **Restrict subjects** per user via NATS permissions to prevent unauthorized
   publishes/subscribes.

## Verified end-to-end flow

1. NATS server starts with `websocket {}` block → port 8080
2. `remote_agent_adapter.py` connects via `ws://localhost:8080`
3. Delegator sends task to `hub.send.<channel>` with `to: remote-worker`
4. Router routes to `channel.inbox.remote-worker`
5. Adapter picks up task, runs backend, publishes result
6. Router delivers result to `channel.<task_channel>`
7. Delegator receives `completed` event with result
