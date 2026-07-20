# nats-hub — Product Vision & Architecture

## The Big Picture

nats-hub is a **universal agent communication layer**. Any process — AI agent,
human, CLI tool, API bridge, evaluation harness — can publish and subscribe to
typed message channels via NATS. The bus is transport-agnostic: it doesn't care
what's on either end, only that messages flow through it.

The result: a drop-in dependency that gives any Rust project instant, performant,
observable, flexible communication between agents, humans, and services.

```
                         ┌─────────────────────────────┐
                         │       NATS Router            │
                         │   (hub-server)               │
                         │   routes by meta.to           │
                         └──────────┬──────────────────┘
                                    │
              ┌─────────────┬───────┼────────┬──────────────┐
              ▼             ▼       ▼        ▼              ▼
         inbox.agent-1  inbox.agent-2  agents.broadcast  task.<uuid>
         (private DM)   (private DM)   (public feed)     (threaded conv)
              │              │              │                │
    ┌─────────┴────┐  ┌──────┴─────┐  ┌────┴─────┐  ┌───────┴───────┐
    │              │  │            │  │          │  │               │
    ▼              ▼  ▼            ▼  ▼          ▼  ▼               ▼
┌────────┐  ┌────────┐  ┌────────┐  ┌────────┐  ┌────────┐  ┌────────────┐
│Worker 1 │  │Worker 2  │  │Human   │  │Postiz  │  │Eval    │  │Storage     │
│(codex)  │  │(LLM API)│  │Bridge  │  │Bridge  │  │Harness │  │(SurrealDB) │
│         │  │         │  │(Tg/SMS)│  │(social)│  │        │  │            │
│execute  │  │call API │  │forward │  │post to │  │score   │  │async mirror│
│commands │  │publish  │  │to Tg,  │  │social, │  │trajec- │  │from router │
│publish  │  │response │  │publish │  │publish │  │tories  │  │            │
│results  │  │back     │  │replies │  │confirm │  │offline │  │cold path   │
└────────┘  └────────┘  └────────┘  └────────┘  └────────┘  └────────────┘
    │              │           │           │           │           │
    └──────────────┴───────────┴───────────┴───────────┘           │
                         all flow through NATS                     │
                         all persisted to SurrealDB ───────────────┘
                                                              │
                                                              ▼
                                                    ┌──────────────────┐
                                                    │ Queryable History │
                                                    │                  │
                                                    │ query_history()  │
                                                    │ get_thread()     │
                                                    │ find_agents()    │
                                                    │ list_pending()   │
                                                    │                  │
                                                    │ (offline evals,  │
                                                    │  observability,  │
                                                    │  audit, analytics)│
                                                    └──────────────────┘
```

## Communication Patterns

### Pattern 1: Broadcast / Feed (Twitter-style)

One-to-many. Anyone subscribed to a channel receives every message.

```
hub-publish --channel agents.broadcast --from hermes --message "new convention: use Rust 2024"
```

```
channel.agents.broadcast
    ├── agent-1 (sees everything)
    ├── agent-2 (sees everything)
    └── agent-3 (sees everything)
```

**Use cases**: Announcements, group coordination, "hey everyone, new plan."
**Current status**: ✅ Implemented and working.

---

### Pattern 2: Inbox / Direct Messaging (DM-style)

One-to-one. The router checks `Envelope.meta.to` and routes to a private inbox
channel instead of the broadcast channel.

```
hub-publish --to worker-1 --from hermes --channel agents.tasks --json '{"prompt":"implement phase 3"}'
```

```
                    hub.send.agents.tasks
                        │
                   NATS Router
                   ┌────┴────┐
                   │ meta.to?│
                   └────┬────┘
              ┌─────────┴──────────┐
              ▼                    ▼
    channel.inbox.worker-1    channel.agents.tasks
    (private, only worker-1   (broadcast, everyone
     receives this)            on agents.tasks)
```

Each agent subscribes to its own `channel.inbox.<identity>`. If `meta.to` is set,
the router routes to `channel.inbox.<to>`. If `meta.to` is null, broadcast to
`channel.<channel>` as normal.

**Use cases**: Task delegation, private 1:1 communication, steering an agent.
**Current status**: ✅ Implemented. The router routes on `meta.to` to
`channel.inbox.<to>` (see `Decision 1`); `hub-delegate` and `hub-publish --to`
both exercise this path.

---

### Pattern 3: Task / Conversation Channels (threaded DMs)

One-to-one with a dedicated channel per conversation. Each task gets a unique
channel, isolating it from all other traffic.

```
hub-delegate --to worker-1 --from hermes --prompt "implement phase 3"
    → generates task-uuid (e.g. "a3f7b2c1")
    → publishes task to channel.inbox.worker-1 with meta.reply_to = "task.a3f7b2c1"
    → worker subscribes to channel.task.a3f7b2c1
    → bidirectional conversation on that channel
    → I subscribe/unsubscribe as needed
    → nobody else on the bus sees any of this
```

```
channel.task.a3f7b2c1
    ├── orchestrator (sent the task, watching for results)
    └── worker-1     (received the task, working on it, publishing status)
```

**Use cases**: Isolated task execution, parallel workstreams, conversation
tracking, offline evaluation of agent trajectories.
**Current status**: ✅ Implemented. `hub-delegate` creates `task.<uuid>` channels,
routes the task to the worker's inbox (`meta.to`), and subscribes to the task
channel for status + reply. Workers reply via `meta.to = <sender>` so results
land in the sender's inbox.

### Pattern comparison

| | Broadcast | Inbox/DM | Task Channel |
|---|---|---|---|
| **Recipients** | All subscribers | One specific agent | Two parties on a private channel |
| **Channel naming** | `<arbitrary>` | `inbox.<identity>` | `task.<uuid>` |
| **meta.to** | null (broadcast) | set (routes to inbox) | set (routes to inbox, reply_to points to task channel) |
| **Isolation** | None — everyone sees everything | Private — only the recipient | Private — only participants |
| **Concurrency** | N/A | Multiple agents each have their own inbox | Multiple tasks run in parallel on separate channels |
| **Best for** | Announcements, group coordination | Task delegation, steering | Deep work, long-running tasks, evaluation |

## Status Checking: Warm vs Cold

Two ways to check on an agent's progress:

| | Live subscription (warm) | Database query (cold) |
|---|---|---|
| **Latency** | Real-time, instant | ~1ms (local SurrealDB) |
| **Context cost** | Orchestrator receives every message on the channel | Only pulls what's needed when asked |
| **Best for** | Actively watching a worker, want live updates, ready to intervene | Casual "how's it going?" check without subscribing |
| **Analogy** | Watching CLI output in real-time | Scrolling back through chat history |
| **Implementation** | `subscribe_channel()` on HubClient | `query_history()` on Storage trait |

**Both paths are always available.** The async mirror in the router writes every
envelope to SurrealDB regardless. The orchestrator chooses: subscribe live for
active supervision, or query on-demand for passive checking.

## The Universal Worker (`hub-worker`)

The plug-and-play component. It subscribes to a channel, receives task envelopes,
executes them, and publishes results back. It doesn't know what the payload means
— it only knows how to extract the relevant field and pass it to an execute command.

```
hub-worker --identity worker-1 \
           --channel inbox.worker-1 \
           --nats-url nats://localhost:4222 \
           --execute "codex"
```

### Worker lifecycle

```
1. Subscribe to channel.inbox.<identity>
2. Receive envelope
3. Extract payload.prompt (or payload.text, or payload.command)
4. Pass to --execute command via stdin
5. Capture stdout as result
6. Publish status envelope: {"status": "working"}
7. [during execution] check for new envelopes (steering messages)
8. Publish result envelope to reply channel (meta.to = original sender)
9. Publish status envelope: {"status": "done"}
10. Loop back to step 2
```

### Plug-and-play: swap the execute command

| `--execute` flag | What the worker does |
|---|---|
| `codex` | Shells out to Codex CLI with the prompt |
| `hermes` | Shells out to Hermes Agent CLI |
| `python worker.py` | Runs a Python script (calls LLM API, processes data, etc.) |
| `postiz post` | Posts to social media via Postiz CLI |
| `cat` | Just echoes the input (testing/debugging) |
| Custom binary | Anything that reads stdin and writes stdout |

The payload schema is the contract between sender and receiver, not the bus:

| Receiver | Expected payload |
|---|---|
| LLM worker | `{"prompt": "...", "model": "..."}` |
| Postiz worker | `{"platform": "twitter", "content": "..."}` |
| Codex worker | `{"prompt": "...", "repo": "/path/to/repo"}` |
| Human bridge (Telegram) | `{"message": "...", "chat_id": 123}` |
| Eval harness | `{"trajectory_id": "...", "criteria": "..."}` |

All flow through the same bus, all get persisted to SurrealDB, all are queryable.

## Human Bridges

Humans participate through bridge workers — specialized `hub-worker` instances
that translate between NATS envelopes and a human-facing transport.

### Telegram bridge (example)

```
hub-worker --identity human-bridge-telegram \
           --channel inbox.telegram-bot \
           --execute "python telegram_bridge.py"
```

```
User sends Telegram message
    → telegram_bridge.py publishes envelope to inbox.<recipient-agent>
    → agent receives, processes, publishes result to inbox.telegram-bot
    → telegram_bridge.py receives result, sends Telegram reply to user
```

The bridge is bidirectional: human → NATS → agent → NATS → human. The agent
doesn't know it's talking to a human on Telegram. It just sees envelopes.

### Other bridges (same pattern)

| Transport | Bridge implementation |
|---|---|
| Telegram | `python telegram_bridge.py` (or Rust binary) |
| SMS (Twilio) | `python sms_bridge.py` |
| WhatsApp | `python whatsapp_bridge.py` |
| Email (IMAP/SMTP) | `python email_bridge.py` |
| Slack | `python slack_bridge.py` |
| Discord | `python discord_bridge.py` |
| Social media (Postiz) | `postiz post` via hub-worker |

Each bridge is a thin adapter. The bus stays agnostic.

## Persistence & Observability

### What's persisted (already wired)

Every envelope that flows through the router is async-mirrored to SurrealDB:

| Data | Where | Query method |
|---|---|---|
| Message history | `envelopes` table | `query_history()` |
| Agent registry | `agents` table | `find_agents()`, `get_agent()` |
| Conversation threads | `reply_to` graph edges | `get_thread()` |
| Pending messages | `envelopes` with no reply | `list_pending()` |

### What this enables

| Use case | How |
|---|---|
| "Check on the agent" | `query_history(channel="task.a3f7b2c1")` → see full trajectory |
| "What's agent-1 working on?" | `list_pending("agent-1")` → unanswered messages |
| "Show me the conversation" | `get_thread(root_id)` → full reply chain |
| "Which agents are alive?" | `find_agents(alive_within=60)` → liveness filter |
| "What did the agent do wrong?" | `query_history()` → full audit trail for debugging |
| Agent evaluation | Query history offline, score trajectories, compare approaches |
| Analytics | Message rates, latency, error rates, channel hotspots (Phase 4) |

### Hot path vs cold path

```
HOT PATH (real-time):
  Agent → hub.send.<channel> → Router → channel.<name> → Subscribers
  Sub-millisecond. No DB I/O. This is the delivery path.

ASYNC MIRROR (write to DB):
  Router → tokio::spawn → Storage::store_envelope()
  Fire-and-forget. If DB write fails, delivery is unaffected.

COLD PATH (query from DB):
  query_history() / find_agents() / get_thread() / list_pending()
  ~1ms (local SurrealDB). Used for observability, debugging, evaluation.
```

## Architecture Decisions

### Decision 1: Router respects `meta.to` (inbox routing)

**Status**: ✅ Implemented in `router.rs`.

If `meta.to` is set, route to `channel.inbox.<to>` instead of `channel.<channel>`.
If `meta.to` is null, broadcast to `channel.<channel>` as we do now.

### Decision 2: Workers subscribe to their inbox by default

**Status**: ✅ Implemented in `hub-worker`, `worker_runtime.py`, and typed Python workers.

A worker subscribes to `channel.inbox.<identity>` (private messages addressed to
it) and optionally to one or more broadcast channels. It does not receive
messages addressed to other agents.

### Decision 3: Reply routing is the worker's responsibility

**Status**: Will be implemented in `hub-worker`.

When a worker publishes a result, it sets `meta.to = <original sender identity>`
so the result goes to the sender's inbox, not to a broadcast channel. The worker
extracts the sender from the incoming envelope's `meta.from`.

### Decision 4: Channel naming conventions

**Status**: Convention only, no code change needed. NATS and the router don't
care about naming — it's all just `channel.<name>`.

| Pattern | Channel naming | Example |
|---|---|---|
| Inbox/DM | `inbox.<identity>` | `inbox.worker-1` |
| Task conversation | `task.<uuid>` | `task.a3f7b2c1` |
| Broadcast | `<arbitrary>` | `agents.broadcast`, `system.announcements` |
| Status | `status.<identity>` | `status.worker-1` (optional, for heartbeat-style updates) |
| Logs | `logs.<identity>` | `logs.worker-1` (optional, for verbose output) |

### Decision 5: Payload schemas are contracts, not enforced by the bus

**Status**: By design, already implemented. `Envelope.payload` is free-form JSON.

The bus doesn't validate payload structure. Each sender/receiver pair agrees on
a schema. This is what makes it plug-and-play — different worker types can use
different payload shapes without changing the bus.

### Decision 6: SurrealDB stays behind the Storage trait

**Status**: Already implemented. BSL safeguard.

Third parties never get direct SurrealDB access. They interact with nats-hub's
messaging API (publish/subscribe/query). The Storage trait abstracts the DB.
This keeps us clear of the BSL DBaaS restriction even when hosting nats-hub as
a service.

## Build Plan

### Phase 2.5: Inbox routing + universal worker (dogfooding unlock) ✅

| Component | Status |
|---|---|
| Router `meta.to` routing | ✅ |
| `hub-worker` binary | ✅ |
| `hub-history` CLI | ✅ |
| `HubClient` reply helpers | ✅ |
| `hub-delegate` + task channels | ✅ |
| Tests | ✅ |

After this phase:
- I can delegate tasks to workers via NATS (not Hermes delegate_task)
- Workers can be any CLI tool, LLM API caller, or bridge
- I can check on worker status via live subscription or DB query
- I can steer workers mid-task via inbox messages
- Everything is persisted to SurrealDB for observability

### Phase 3: Stateful orchestration (sessions, events, waves) ✅

Implemented per [`docs/PHASE3_PLAN.md`](PHASE3_PLAN.md). nats-hub now supports:

| Capability | CLI / module |
|---|---|
| Multi-turn sessions | `hub-session`, `worker_runtime` session mode |
| Structured event streams | `hub-watch`, `src/events/`, `worker_events.py` |
| Parallel wave execution | `hub-wave`, `src/wave/`, `src/storage/wave.rs` |

**Remaining from original vision:**
- Human bridges (Phase 5 below)
- Multi-project portability / crate packaging (Phase 6 below)

### Phase 4: Analytics trait + observability ✅

Implemented across three deliverables:

| Sub-phase | Deliverable | Status |
|---|---|---|
| **4a** | `Analytics` trait + `SurrealAnalytics` (reads from `envelopes` history) + `hub-stats` CLI | ✅ |
| **4b** | `MetricsCollector` (atomic, hot-path-safe) + `hub-server --metrics-addr` Prometheus-compatible endpoint | ✅ |
| **4c** | DuckDB OLAP backend (optional/stretch) | 📋 Deferred — trait is backend-agnostic; add when heavy analytical workloads appear |

`hub-stats` answers message rates, latency, agent activity, channel hotspots, and
error rate against the persisted history. `hub-server --metrics-addr <addr>`
serves a zero-dependency Prometheus exposition format (`natshub_messages_total`,
`natshub_messages_by_kind`, `natshub_messages_by_channel_class`,
`natshub_errors_total`) with bounded labels — safe on the routing hot path.

### Phase 7: Visualizer + TUI

Two surfaces for observing and steering agent work:

**Arcade Visualizer** (HTML/p5.js, served by hub-server's WS bridge) — **shipped**:
- Retro CRT aesthetic: scanlines, neon grid, pixel-art agent characters
- Each agent = a square with glow, trail, status color, session_id label
- Thought bubbles show real stdout/event snippets
- Click agent → action popup (message, view session, stop, resume)
- Particle bursts on task completion/error/start
- Hooks: petdex sprite integration (same tech as Hermes/Codex pets)
- WebSocket: hub-server pushes every routed envelope to all browser clients

**TUI** (ratatui, `hub-tui` binary) — **planned, not implemented**:
- Clean modern daily driver — keyboard-driven, not bloated
- Live agent dashboard, sessions, wave status, message feed
- Subscribes to NATS directly + uses query API for persistent data
- Spec: `docs/TUI_PLAN.md`

### Phase 5: Human bridges — **partial**

- **Shipped:** `telegram_bridge.py`, `discord_bridge.py` (standalone inbox adapters; dry-run without SDK tokens). See `docs/BRIDGES.md`.
- **Not shipped:** SMS, Slack, Email, Postiz — copy the same bridge shape.

### Phase 6: Multi-project portability

Package nats-hub as a portable crate dependency. Feature flags for storage
backends. Documentation for embedding in other Rust projects (`docs/PORTABILITY.md`).

### Distributed teams + auth (cross-cutting, shipped July 2026)

- NATS native WebSocket + remote adapter (`docs/REMOTE_AGENTS.md`)
- Token/TLS on Python (`nats_connect`) and Rust (`HubConnectOptions` / env)
- Operator/join docs: `docs/SECURITY.md`, `OPERATOR_HUB.md`, `JOIN_HUB.md`, `REMOTE_INSTALL.md`
- Dogfood: `scripts/dogfood_token_auth.sh`, `scripts/dogfood_wss_tls.sh`

## What nats-hub is NOT

- ❌ **Not an LLM** — it's a communication layer. Workers call LLMs; the bus
  doesn't.
- ❌ **Not an agent framework** — it doesn't manage agent lifecycles, prompt
  chains, or tool calling. Workers do that. The bus moves messages.
- ❌ **Not an agent memory store** — that's Agent Memory Labs' job. nats-hub's
  DB stores messaging metadata (history, registry, threads), not agent knowledge.
- ❌ **Not a DBaaS** — SurrealDB is always behind the Storage trait. Third
  parties never touch it directly (BSL safeguard).
- ❌ **Not a replacement for HTTP/gRPC** — it's for agent-to-agent and
  agent-to-human messaging, not for serving web requests.

## File Layout (current + planned)

```
src/
├── lib.rs                    — module root, re-exports
├── protocol.rs               — Envelope, Meta, MessageKind, subjects
├── client.rs                 — HubClient + AgentRegistry (in-memory cache)
├── events/                   — structured progress events (Phase 3b)
├── wave/                     — wave validation + spawn orchestration (Phase 3c)
├── router.rs                 — ControlPlane (routing daemon, async DB mirror)
├── storage/
│   ├── mod.rs                — Storage trait + query types
│   ├── surreal.rs            — SurrealStorage impl (embedded RocksDB)
│   ├── session.rs            — session CRUD (Phase 3a)
│   └── wave.rs               — wave + wave_tasks CRUD (Phase 3c)
├── analytics/                — Phase 4 observability
│   ├── mod.rs                — Analytics trait (read-side peer to Storage)
│   ├── surreal.rs            — SurrealAnalytics impl (reads envelopes history)
│   └── metrics.rs            — MetricsCollector (atomic, hot-path-safe)
└── bin/
    ├── hub_server.rs         — runs the control plane router (daemon)
    ├── hub_publish.rs        — send a message on a channel (broadcast or --to DM)
    ├── hub_observe.rs        — watch messages on channels (read-only)
    ├── hub_interact.rs       — interactive REPL for human messaging
    ├── hub_register.rs       — register an agent with capabilities
    ├── hub_agents.rs         — list/search registered agents
    ├── hub_worker.rs         — universal worker: subscribe, execute, reply
    ├── hub_history.rs        — query message history from DB
    ├── hub_delegate.rs       — task channel + delegate to a worker
    ├── hub_session.rs        — stateful multi-turn sessions (Phase 3a)
    ├── hub_watch.rs          — watch structured progress events (Phase 3b)
    ├── hub_wave.rs           — parallel wave orchestration (Phase 3c)
    ├── hub_thread.rs         — view conversation threads + pending messages
    └── hub_stats.rs          — observability / analytics CLI (Phase 4a)

worker_runtime.py             — shared Python worker (oneshot + session + wave)
worker_events.py              — structured event publishing
worker_backends/              — HeadlessCli, SdkAgent, AcpAgent backends
```
