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
| `kilo` | `--model <model>` | `kilo run --format json --auto` headless CLI |
| `opencode` | `--model <model>` | `opencode run --format json` headless CLI |

To add a new named backend, add a spec to `worker_backends/presets.py` and a
case in `remote_agent_adapter.py::build_backend()`.

## Security

The checked-in configuration is intentionally development-only: `no_tls: true`
accepts cleartext `ws://` connections and the active file has no credentials.
Do not expose that listener to an untrusted network. A production deployment
should use `wss://`, authenticate every client, and grant each identity only the
subjects it needs.

### Authentication examples

For a small deployment, a token provides a single shared credential:

```conf
authorization {
    token: "REPLACE_WITH_A_LONG_RANDOM_TOKEN"
}
```

For individually revocable credentials, configure users. Restrict a remote
agent's credential to WebSocket connections so it cannot also authenticate to
the NATS TCP listener:

```conf
authorization {
    users: [
        {
            user: "remote-agent-1"
            password: "REPLACE_WITH_A_LONG_RANDOM_PASSWORD"
            allowed_connection_types: ["WEBSOCKET"]
        }
    ]
}
```

Store real credentials in deployment secrets, not in Git. NKEY/JWT-based auth
is preferable when operating a larger multi-tenant deployment and needing
credential rotation or account isolation.

### Subject-level permissions

NATS permissions are allowlists. This example lets `remote-agent-1` receive only
its routed inbox and a specific task channel, and publish only messages bound
for that task plus its registration, presence, and event traffic. Replace the
identity and task channel with the exact values assigned to the worker:

```conf
authorization {
    users: [
        {
            user: "remote-agent-1"
            password: "REPLACE_WITH_A_LONG_RANDOM_PASSWORD"
            allowed_connection_types: ["WEBSOCKET"]
            permissions: {
                publish: {
                    allow: [
                        "hub.send.task.remote-agent-1"
                        "hub.register"
                        "hub.presence"
                        "hub.events.>"
                    ]
                }
                subscribe: {
                    allow: [
                        "channel.inbox.remote-agent-1"
                        "channel.task.remote-agent-1"
                    ]
                }
            }
        }
    ]
}
```

If the assigned workflow uses request/reply, add only the specific inbox prefix
required by that client. Avoid a broad `_INBOX.>` subscription in sensitive
deployments because it can expose other clients' replies.

### Production Setup

The following procedure creates a private key and a self-signed certificate for
an initial deployment. For an Internet-facing service, use a certificate issued
by a trusted CA (for example, an ACME client) instead. Substitute the public DNS
name clients will actually connect to.

1. Create a protected certificate directory and generate the key/certificate:

   ```bash
   sudo install -d -m 0750 -o nats -g nats /etc/nats/tls
   sudo openssl req -x509 -newkey rsa:4096 -sha256 -nodes \
       -days 365 \
       -keyout /etc/nats/tls/nats-server.key \
       -out /etc/nats/tls/nats-server.crt \
       -subj "/CN=hub.example.com" \
       -addext "subjectAltName=DNS:hub.example.com"
   sudo chown nats:nats /etc/nats/tls/nats-server.key /etc/nats/tls/nats-server.crt
   sudo chmod 0600 /etc/nats/tls/nats-server.key
   sudo chmod 0644 /etc/nats/tls/nats-server.crt
   ```

2. Replace `no_tls: true` in the WebSocket block and enable one auth block. This
   example uses token auth; generate the token with a cryptographically secure
   secret generator and inject it through deployment tooling:

   ```conf
   websocket {
       port: 8080
       cert_file: "/etc/nats/tls/nats-server.crt"
       key_file: "/etc/nats/tls/nats-server.key"
       same_origin: false
       handshake_timeout: "5s"
   }

   authorization {
       token: "REPLACE_WITH_A_LONG_RANDOM_TOKEN"
   }
   ```

   For per-agent permissions, use the `users` example above instead of the token
   block. Do not configure both examples as if they were additive alternatives.

3. Validate before restarting, then restart using the service manager for the
   deployment:

   ```bash
   nats-server -t -c config/nats-server.conf
   sudo systemctl restart nats-server
   ```

4. For a self-signed/private-CA certificate, securely copy the CA certificate
   (the generated certificate itself in this example) to the client. Never set
   `tls_insecure` merely to bypass verification. A minimal `nats-py` token/TLS
   client is:

   ```python
   import asyncio
   import ssl
   import nats

   async def main():
       tls_context = ssl.create_default_context(cafile="/etc/nats/ca/nats-server.crt")
       nc = await nats.connect(
           servers=["wss://hub.example.com:8080"],
           token="REPLACE_WITH_A_LONG_RANDOM_TOKEN",
           tls=tls_context,
       )
       await nc.publish("hub.send.task.remote-agent-1", b'{"message":"ready"}')
       await nc.flush()
       await nc.close()

   asyncio.run(main())
   ```

   With a publicly trusted certificate, `ssl.create_default_context()` can use
   the operating system trust store. The server name in the `wss://` URL must
   match a certificate SAN.

5. Connect the remote adapter to the secure endpoint. The current adapter does
   not yet expose token/TLS-file CLI flags, so production deployment must first
   wire the equivalent `nats-py` options (`token=...` and `tls=tls_context`) into
   its connection configuration. Once available, the intended invocation is:

   ```bash
   python3 remote_agent_adapter.py \
       --identity remote-agent-1 \
       --nats-url wss://hub.example.com:8080 \
       --token "$NATS_TOKEN" \
       --backend shell \
       --execute "my-agent-cli --prompt"
   ```

   Do not deploy this command unchanged until `remote_agent_adapter.py --help`
   lists the token and CA/TLS options; editing that adapter is outside this
   security-documentation task's ownership.

## Verified end-to-end flow

1. NATS server starts with `websocket {}` block → port 8080
2. `remote_agent_adapter.py` connects via `ws://localhost:8080`
3. Delegator sends task to `hub.send.<channel>` with `to: remote-worker`
4. Router routes to `channel.inbox.remote-worker`
5. Adapter picks up task, runs backend, publishes result
6. Router delivers result to `channel.<task_channel>`
7. Delegator receives `completed` event with result
