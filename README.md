# nats-hub

A NATS-based communication layer with a control plane router for agent-to-agent and human-to-agent messaging. Built in Rust with `async-nats`.

## Architecture

```
 ┌───────────────┐    ┌───────────────────┐    ┌───────────────┐
 │  Agent Client  │──▶│   NATS Server     │◀──│  Agent Client  │
 │  (publisher)   │    │   (core NATS)     │    │  (subscriber)  │
 └───────────────┘    └───────┬───────────┘    └───────────────┘
                              │
                       ┌──────▼───────┐
                       │  Control     │
                       │  Plane Worker│  (subscribes to hub.send.>,
                       │  (router)    │   routes to channel.<name>)
                       └──────┬───────┘
                              │
         ┌────────────┬───────┼────────┐
         ▼            ▼                ▼
   channel.agentA  channel.agentB  channel.humans
```

**Flow:**
1. Agents publish messages to `hub.send.<channel>` via `hub-publish` or the `HubClient` API
2. The control plane router (`hub-server`) subscribes to `hub.send.>` and routes each message to `channel.<channel>`
3. Subscribers (agents, humans, observers) subscribe to `channel.<name>` or `channel.>` (all)
4. Humans can interact via `hub-interact` (REPL) or observe via `hub-observe`

## Quick Start

```bash
# 1. Start the NATS server
nats-server -c config/nats-server.conf

# 2. Start the control plane router (in another terminal)
cargo run --release --bin hub-server
# or: ./target/release/hub-server

# 3. Register an agent
./target/release/hub-register --identity agent-alpha --capabilities compute,observe

# 4. Publish a message
./target/release/hub-publish --channel agents.broadcast --from agent-alpha --message "Hello!"

# 5. Observe messages (in another terminal)
./target/release/hub-observe --channel agents.broadcast
# or observe all channels:
./target/release/hub-observe

# 6. Interactive human mode
./target/release/hub-interact --from josh --channel agents.broadcast
```

## CLI Tools

| Binary | Description |
|--------|-------------|
| `hub-server` | Runs the control plane router (daemon) |
| `hub-publish` | Send a message on a channel |
| `hub-observe` | Watch messages on channels (human observer) |
| `hub-interact` | Interactive REPL for human messaging |
| `hub-register` | Register an agent with capabilities |

## Wire Protocol

Every message is a JSON `Envelope`:

```json
{
  "meta": {
    "id": "uuid",
    "from": "agent-alpha",
    "channel": "agents.broadcast",
    "to": null,
    "timestamp": "2026-06-29T12:00:00Z",
    "kind": "message",
    "reply_to": null
  },
  "payload": { "text": "Hello!" }
}
```

**Message kinds:** `message`, `control`, `human`, `status`

## Subject Convention

| Subject | Direction | Purpose |
|---------|-----------|---------|
| `hub.send.<channel>` | Agent → Router | Agents publish here |
| `channel.<name>` | Router → Subscribers | Routed messages arrive here |
| `channel.>` | Router → Observer | Wildcard catch-all |
| `hub.register` | Agent → Router | Agent registration |
| `hub.presence` | Agent → Router | Heartbeat |

## Using the Library

```rust
use nats_hub::{HubClient, Envelope, MessageKind};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = HubClient::connect("nats://127.0.0.1:4222", "agent-alpha").await?;
    
    // Register
    client.register(vec!["compute".into()]).await?;
    
    // Send a message
    client.send_message("agents.broadcast", serde_json::json!({"text": "hello"})).await?;
    
    // Subscribe to a channel
    let mut rx = client.subscribe_channel("agents.broadcast").await?;
    while let Some(env) = rx.recv().await {
        println!("Got: {:?}", env);
    }
    
    Ok(())
}
```

## Building

```bash
cargo build --release
```

Binaries are in `target/release/`.

## NATS Server

To install the NATS server:
```bash
# Download from https://github.com/nats-io/nats-server/releases
# Or via Docker:
docker run -p 4222:4222 -p 8222:8222 nats:latest -c /etc/nats/nats-server.conf
```

Monitoring endpoint: http://127.0.0.1:8222/varz
