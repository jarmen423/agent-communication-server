# Phase 3: Stateful Sessions + Event Streams + Wave Orchestration

## Status

| Sub-phase | Status | Completed |
|---|---|---|
| **3a** Stateful Sessions | ✅ Done | `hub-session`, `SessionRecord`, `worker_runtime` session mode |
| **3b** Event Stream + `hub-watch` | ✅ Done | `MessageKind::Event`, `src/events/`, `worker_events.py`, `hub-watch` |
| **3c** Wave Orchestration | ✅ Done | `hub-wave`, `WaveRecord`/`WaveTaskRecord`, `src/wave/`, wave worker mode |

**Verification (all sub-phases):** `CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats cargo test` — 39 tests pass; `cargo build` clean.

## Overview

Transform nats-hub from a one-shot delegation bus into a stateful agent
orchestration platform. Three sub-phases, each building on the last.

**Inspired by:** the wave-execution skill's methodology (disjoint write
scopes, handoffs, merge gates) — but implemented with NATS channels +
SurrealDB instead of file-based task registries.

**Also incorporates:** stateful session semantics from the
`subagent-tool` prototype (multi-turn conversations, event replay,
watch) — but provider-agnostic and DB-backed.

---

## Phase 3a: Stateful Sessions ✅

> **Implemented.** See `src/bin/hub_session.rs`, `src/storage/session.rs`, `tests/sessions.rs`.

### Concept

A session is a persistent, multi-turn conversation between an orchestrator
and a worker. Unlike `hub-delegate` (one-shot: send → reply → done), a
session stays alive: the orchestrator can send follow-up messages, steer
mid-task, and close when done.

### Subject Conventions

| Subject | Purpose |
|---|---|
| `channel.session.<uuid>` | Session broadcast channel — all messages for this session |
| `channel.inbox.<worker>` | Worker inbox — where session creation requests arrive |

Session creation flow:
1. Orchestrator DMs worker on `channel.inbox.<worker>` with
   `payload.action = "session_start"`, `payload.session_id = "<uuid>"`
2. Worker subscribes to `channel.session.<uuid>`, replies with
   `status: ready`
3. Orchestrator sends follow-up messages on `channel.session.<uuid>`
   (broadcast — worker is subscribed)
4. Worker publishes results + status on same channel
5. Orchestrator (or worker) sends `payload.action = "session_close"`

### Schema (SurrealDB)

New `sessions` table:

```sql
DEFINE TABLE sessions SCHEMALESS;
DEFINE FIELD session_id    AT sessions TYPE string;
DEFINE FIELD orchestrator  AT sessions TYPE string;
DEFINE FIELD worker        AT sessions TYPE string;
DEFINE FIELD status        AT sessions TYPE string;  -- active, idle, closed
DEFINE FIELD cwd           AT sessions TYPE option<string>;
DEFINE FIELD model         AT sessions TYPE option<string>;
DEFINE FIELD provider      AT sessions TYPE option<string>;
DEFINE FIELD created_at    AT sessions TYPE datetime;
DEFINE FIELD updated_at    AT sessions TYPE datetime;
DEFINE FIELD closed_at     AT sessions TYPE option<datetime>;
DEFINE FIELD metadata      AT sessions TYPE object;
DEFINE INDEX idx_sessions_status   ON TABLE sessions COLUMNS status;
DEFINE INDEX idx_sessions_worker   ON TABLE sessions COLUMNS worker, status;
```

### Storage Trait Additions (`src/storage/mod.rs`)

```rust
// ── Sessions ─────────────────────────────────────────────

/// Persisted session record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub orchestrator: String,
    pub worker: String,
    pub status: String,         // "active", "idle", "closed"
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

// Add to Storage trait:
async fn create_session(&self, session: SessionRecord) -> Result<()>;
async fn update_session_status(&self, session_id: &str, status: &str) -> Result<()>;
async fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>>;
async fn list_sessions(&self, filter: &SessionFilter) -> Result<Vec<SessionRecord>>;
```

### New Filter Type

```rust
#[derive(Debug, Clone, Default)]
pub struct SessionFilter {
    pub status: Option<String>,       // "active", "closed"
    pub worker: Option<String>,
    pub orchestrator: Option<String>,
    pub limit: Option<usize>,
}
```

### SurrealStorage Implementation (`src/storage/surreal.rs`)

- Add `SessionRow` struct (like `AgentRow` / `EnvelopeRow`)
- Implement `create_session`, `update_session_status`, `get_session`,
  `list_sessions` following the same pattern as agent/envelope methods
- Add session table to `migrate()`

### HubClient Additions (`src/client.rs`)

```rust
/// Start a session with a worker. DMs the worker with a session_start
/// action and returns the session UUID.
pub async fn start_session(
    &self,
    worker: &str,
    payload: serde_json::Value,
) -> Result<String>;

/// Send a follow-up message on an existing session channel.
pub async fn send_to_session(
    &self,
    session_id: &str,
    payload: serde_json::Value,
) -> Result<String>;

/// Close a session (sends session_close action).
pub async fn close_session(
    &self,
    session_id: &str,
) -> Result<()>;

/// Subscribe to a session channel (for watching events).
pub async fn subscribe_session(
    &self,
    session_id: &str,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<Envelope>>;
```

### New CLI: `hub-session` (`src/bin/hub_session.rs`)

```
hub-session create --worker <agent> --from <id> [--model M] [--provider P] [--cwd PATH] [--prompt MSG]
hub-session send <session-id> --from <id> --message MSG
hub-session close <session-id> --from <id>
hub-session list [--status active|closed] [--worker <agent>] [--limit N]
hub-session status <session-id>
```

`create` flow:
1. Generate session UUID
2. Persist to DB via `Storage::create_session()`
3. DM the worker on `channel.inbox.<worker>` with
   `payload = {action: "session_start", session_id, prompt?, model?, provider?, cwd?}`
4. Subscribe to `channel.session.<uuid>` and wait for `status: ready`
5. Print session ID

`send` flow:
1. Publish on `channel.session.<uuid>` with `payload = {action: "session_send", message}`
2. Update `updated_at` in DB

`close` flow:
1. Publish on `channel.session.<uuid>` with `payload = {action: "session_close"}`
2. Update status to `closed` in DB

### Worker Changes

Workers (hermes_worker.py, cursor_worker.py) need a `--session-mode` flag
that changes their behavior:

**Current (one-shot):**
- Subscribe to inbox only
- Receive task → execute → publish result → done

**Session mode:**
- Subscribe to inbox (for new session requests)
- On `session_start`: subscribe to `channel.session.<uuid>`,
  publish `status: ready`, then loop listening on that channel
- On `session_send`: execute the prompt, publish result on session channel
- On `session_close`: unsubscribe from session channel, publish `status: closed`

The session-mode worker stays alive and can handle multiple sessions
concurrently (each session gets its own asyncio task listening on its
channel).

### Cargo.toml

Add `[[bin]]` entry for `hub-session`.

### Tests (`tests/sessions.rs`)

1. `test_session_create_and_close` — create session in DB, verify
   status transitions active → closed
2. `test_session_filter` — create multiple sessions, filter by status
   and worker
3. `test_session_persistence` — create session, reopen DB, verify
   session survives restart
4. `test_session_channel_isolation` — (requires NATS) two sessions,
   messages don't cross-contaminate

### File Impact

| File | Change | Est. LOC |
|---|---|---|
| `src/storage/mod.rs` | `SessionRecord`, `SessionFilter`, trait methods | ~60 |
| `src/storage/surreal.rs` | `SessionRow`, impl, migrate additions | ~120 |
| `src/client.rs` | `start_session`, `send_to_session`, `close_session`, `subscribe_session` | ~60 |
| `src/bin/hub_session.rs` | New CLI binary | ~200 |
| `Cargo.toml` | `[[bin]]` entry | ~3 |
| `tests/sessions.rs` | 4 tests | ~120 |
| `src/lib.rs` | Re-export `SessionRecord`, `SessionFilter` | ~2 |
| **Total** | | **~565** |

---

## Phase 3b: Structured Event Stream + `hub-watch` ✅

> **Implemented.** See `src/events/`, `src/bin/hub_watch.rs`, `worker_events.py`, `tests/events.rs`.
> Wave watch uses a `channel_prefix` filter on `channel.>` (not `channel.wave.{id}.>`) so wave-level broadcasts are included.

### Concept

Workers publish typed progress events during execution — not just the
final result. This enables real-time observation via `hub-watch` and
lays the groundwork for the dashboard/TUI vision.

### Protocol Changes (`src/protocol.rs`)

Add `Event` to `MessageKind`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Message,
    Control,
    Human,
    Status,
    Event,  // NEW
}
```

### Event Payload Convention

Events use `MessageKind::Event` with a typed payload:

```json
{
  "event_type": "started|progress|stdout|milestone|completed|error",
  "data": { ... }
}
```

| Event type | When | Data |
|---|---|---|
| `started` | Worker begins processing | `{ "prompt": "..." }` |
| `progress` | Mid-execution update | `{ "message": "..." }` |
| `stdout` | Captured stdout from subprocess | `{ "line": "..." }` |
| `milestone` | Significant step reached | `{ "name": "...", "detail": "..." }` |
| `completed` | Task done | `{ "result": "..." }` |
| `error` | Task failed | `{ "error": "..." }` |

### New CLI: `hub-watch` (`src/bin/hub_watch.rs`)

```
hub-watch --session <uuid>           # watch all events for a session
hub-watch --wave <wave-id>           # watch all events for a wave
hub-watch --agent <identity>         # watch all events from an agent
hub-watch --channel <name>           # watch all events on a channel
hub-watch --all                      # watch everything (channel.>)
```

Pretty-prints events in real-time with color-coded types:

```
[16:42:01] started    hermes-worker-1  session.a3f7b2c1  "What is 7*8?"
[16:42:03] progress   hermes-worker-1  session.a3f7b2c1  "calling model..."
[16:42:05] completed  hermes-worker-1  session.a3f7b2c1  "7*8=56."
```

Implementation: subscribe to the relevant channel(s), filter by
`MessageKind::Event`, pretty-print. Uses `colored` crate (or ANSI codes
directly) for color output.

### Worker Changes

Workers publish events during execution:

```python
# Before executing
await publish_event(nc, session_channel, "started", {"prompt": prompt})

# During execution (if streaming stdout)
await publish_event(nc, session_channel, "stdout", {"line": line})

# On completion
await publish_event(nc, session_channel, "completed", {"result": result})
```

### File Impact

| File | Change | Est. LOC |
|---|---|---|
| `src/protocol.rs` | Add `Event` variant | ~3 |
| `src/bin/hub_watch.rs` | New CLI binary | ~180 |
| `Cargo.toml` | `[[bin]]` entry | ~3 |
| `hermes_worker.py` | Publish events during execution | ~30 |
| `cursor_worker.py` | Publish events during execution | ~30 |
| **Total** | | **~246** |

---

## Phase 3c: Wave Orchestration ✅

> **Implemented.** See `src/bin/hub_wave.rs`, `src/storage/wave.rs`, `src/wave/`, `tests/waves.rs`.
> Workers accept `channel` + `wave_id` on `session_start`; events mirror to both task and wave channels.

### Concept

A wave is a collection of parallel tasks with:
- Disjoint write scopes (each task owns specific files/dirs)
- Dependencies (task B waits for task A)
- Handoffs (task output → next task's input)
- Merge gate (all tasks done + verification passes)

Inspired by the wave-execution skill methodology, but implemented with
NATS channels + SurrealDB instead of file-based task registries.

### Subject Conventions

| Subject | Purpose |
|---|---|
| `channel.wave.<wave-id>` | Wave-level broadcast — manifest, status, handoffs |
| `channel.wave.<wave-id>.task.<task-id>` | Individual task channel |
| `channel.inbox.<worker>` | Worker inbox — where task assignments arrive |

All workers participating in a wave subscribe to `channel.wave.<wave-id>`
for wave-level coordination (status updates, handoff notifications).

### Wave Manifest (SurrealDB)

```sql
DEFINE TABLE waves SCHEMALESS;
DEFINE FIELD wave_id      AT waves TYPE string;
DEFINE FIELD goal         AT waves TYPE string;
DEFINE FIELD status       AT waves TYPE string;  -- pending, running, completed, failed
DEFINE FIELD orchestrator AT waves TYPE string;
DEFINE FIELD created_at   AT waves TYPE datetime;
DEFINE FIELD closed_at    AT waves TYPE option<datetime>;
DEFINE FIELD metadata     AT waves TYPE object;
DEFINE INDEX idx_waves_status ON TABLE waves COLUMNS status;

DEFINE TABLE wave_tasks SCHEMALESS;
DEFINE FIELD wave_id      AT wave_tasks TYPE string;
DEFINE FIELD task_id      AT wave_tasks TYPE string;
DEFINE FIELD worker       AT wave_tasks TYPE string;
DEFINE FIELD goal         AT wave_tasks TYPE string;
DEFINE FIELD status       AT wave_tasks TYPE string;  -- pending, running, done, failed
DEFINE FIELD write_scope  AT wave_tasks TYPE array<string>;
DEFINE FIELD dependencies AT wave_tasks TYPE array<string>;
DEFINE FIELD handoff_path AT wave_tasks TYPE option<string>;
DEFINE FIELD verify_cmd   AT wave_tasks TYPE option<string>;
DEFINE FIELD created_at   AT wave_tasks TYPE datetime;
DEFINE FIELD started_at   AT wave_tasks TYPE option<datetime>;
DEFINE FIELD completed_at AT wave_tasks TYPE option<datetime>;
DEFINE FIELD result       AT wave_tasks TYPE option<string>;
DEFINE INDEX idx_wt_wave_status ON TABLE wave_tasks COLUMNS wave_id, status;
```

### Storage Trait Additions

```rust
// ── Waves ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveRecord {
    pub wave_id: String,
    pub goal: String,
    pub status: String,
    pub orchestrator: String,
    pub created_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveTaskRecord {
    pub wave_id: String,
    pub task_id: String,
    pub worker: String,
    pub goal: String,
    pub status: String,
    pub write_scope: Vec<String>,
    pub dependencies: Vec<String>,
    pub handoff_path: Option<String>,
    pub verify_cmd: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub result: Option<String>,
}

// Add to Storage trait:
async fn create_wave(&self, wave: WaveRecord) -> Result<()>;
async fn update_wave_status(&self, wave_id: &str, status: &str) -> Result<()>;
async fn get_wave(&self, wave_id: &str) -> Result<Option<WaveRecord>>;
async fn list_waves(&self, status: Option<&str>) -> Result<Vec<WaveRecord>>;

async fn create_wave_task(&self, task: WaveTaskRecord) -> Result<()>;
async fn update_wave_task_status(&self, wave_id: &str, task_id: &str, status: &str, result: Option<&str>) -> Result<()>;
async fn get_wave_task(&self, wave_id: &str, task_id: &str) -> Result<Option<WaveTaskRecord>>;
async fn list_wave_tasks(&self, wave_id: &str) -> Result<Vec<WaveTaskRecord>>;
```

### New CLI: `hub-wave` (`src/bin/hub_wave.rs`)

```
hub-wave create --goal "..." --from <id> --tasks tasks.json
    # tasks.json: [{ task_id, worker, goal, write_scope, dependencies, verify_cmd }]
hub-wave spawn <wave-id> [--from <id>]
    # sends session_start to each task's worker on their inbox
    # each task gets channel.wave.<wave-id>.task.<task-id>
hub-wave status <wave-id>
    # prints wave status + all task statuses
hub-wave close <wave-id> [--from <id>]
    # marks wave as completed (or failed if any task failed)
hub-wave list [--status running|completed|failed]
```

`create` flow:
1. Parse tasks JSON
2. Validate: no overlapping write scopes, dependencies reference
   existing task_ids
3. Persist wave + all tasks to DB
4. Publish wave manifest on `channel.wave.<wave-id>` (broadcast)
5. Print wave ID

`spawn` flow:
1. Query wave tasks from DB
2. For each task with no unmet dependencies:
   - DM the task's worker on `channel.inbox.<worker>` with
     `payload = {action: "session_start", session_id: task_id, wave_id,
      channel: "wave.<wave-id>.task.<task-id>", prompt: task.goal,
      write_scope, verify_cmd}`
   - Update task status to `running`
3. For tasks with dependencies: wait for dependency tasks to complete
   (subscribe to their task channels, listen for `completed` events)
4. When a dependency completes, start the dependent task

`status` flow:
1. Query wave + all tasks from DB
2. Pretty-print: wave status, per-task status, completion %

### Merge Gate

A wave is `completed` when:
- All tasks have status `done`
- All `verify_cmd`s passed (worker publishes `event_type: "milestone"`
  with `name: "verify_passed"`)

A wave is `failed` when:
- Any task has status `failed`

`hub-wave close` checks the merge gate and sets the final status.

### Worker Changes

Workers in wave mode:
- Subscribe to `channel.wave.<wave-id>` for wave coordination
- Subscribe to their assigned `channel.wave.<wave-id>.task.<task-id>`
- On task completion, publish `event_type: "completed"` on the task
  channel AND on the wave channel (so the orchestrator and dependent
  tasks know)
- On task failure, publish `event_type: "error"` on both channels

### File Impact

| File | Change | Est. LOC |
|---|---|---|
| `src/storage/mod.rs` | `WaveRecord`, `WaveTaskRecord`, trait methods | ~80 |
| `src/storage/surreal.rs` | `WaveRow`, `WaveTaskRow`, impl, migrate | ~200 |
| `src/client.rs` | Wave helper methods (optional, can use raw send) | ~40 |
| `src/bin/hub_wave.rs` | New CLI binary | ~300 |
| `Cargo.toml` | `[[bin]]` entry | ~3 |
| `tests/waves.rs` | 4 tests | ~150 |
| `src/lib.rs` | Re-exports | ~2 |
| **Total** | | **~775** |

---

## Implementation Order

1. ~~**3a first** (sessions)~~ ✅ — foundation for everything else
2. ~~**3b second** (event stream)~~ ✅ — builds on sessions, needed for wave observation
3. ~~**3c third** (waves)~~ ✅ — builds on both sessions and events

Total estimated: ~1,586 LOC across 3 sub-phases. **All three sub-phases complete.**

## Next (not in Phase 3 scope)

- Analytics / `hub-stats` (Phase 4 in `DATABASE_PLAN.md`)
- Dashboard / TUI consuming `hub-watch` event streams

## Build Conventions

- Set `CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats` for all builds
- Keep files under 400 LOC — split if needed
- All I/O async (tokio + async-nats)
- SurrealDB behind `Storage` trait always
- Tests that need NATS skip gracefully if server isn't running
- Match existing code style (see `protocol.rs`, `client.rs`, `surreal.rs`)

## Verification

After each sub-phase:
1. `CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats cargo test` — all
   tests pass (existing + new)
2. `CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats cargo build` —
   clean build, no warnings
3. Manual dogfood: start NATS + router + worker, run the new CLI,
   verify round-trip
