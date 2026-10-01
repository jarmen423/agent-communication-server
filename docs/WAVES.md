# Wave orchestration

A **wave** is a group of parallel tasks with disjoint write scopes and
dependencies between them. Orchestration runs **inside hub-server** — there
is no client-side spawn loop, so waves survive client exits and hub-server
restarts alike.

```
hub-wave create ...         hub-wave spawn <id>          hub-wave status <id>
        │                          │                            │
        └──────────────►  hub.api (request-reply)  ◄────────────┘
                                 │
                     hub-server wave orchestrator
                     ├── validates + persists (waves, wave_tasks)
                     ├── DMs session_start to each ready task's worker
                     ├── subscribes channel.wave.> + hub.presence
                     ├── liveness TTL → dead worker ⇒ task failed
                     └── fail-fast: first task failure fails the wave
```

## `hub.api` ops

All ops go over the existing request-reply API (`hub.api.<op>`,
`{ok, data|error}` envelopes). The wave ops require a hub-server running
with storage (`--db-path`); without the orchestrator they return an error.

### `wave.create`

Atomic create — preferred form:

```json
{
  "op": "wave.create",
  "params": {
    "wave":  {"wave_id": "w1", "goal": "...", "status": "pending",
              "orchestrator": "me", "created_at": "<RFC3339>", "metadata": {}},
    "tasks": [{"task_id": "t1", "worker": "w-1", "goal": "...",
               "write_scope": ["src/a"], "dependencies": [],
               "handoff_path": "...", "verify_cmd": "cargo test -p a"}]
  }
}
```

The full task list is validated **before anything is persisted**: write-scope
overlap, duplicate task ids, unknown dependencies, and **dependency cycles**
(a cycle previously hung the client-side loop forever; it is now rejected at
create time). `tasks` may be omitted for a wave-only create; the legacy form
(bare `WaveRecord` as `params`) still works.

### `wave.spawn`

```json
{"op": "wave.spawn", "params": {"wave_id": "w1", "timeout_secs": 3600}}
```

Hands the wave to the orchestrator: marks it `running`, records the timeout
in `wave.metadata.timeout_secs`, and DMs `session_start` to the worker of
every ready task on its task channel `wave.<id>.task.<task_id>` (same payload
shape as before: `action/session_id/wave_id/channel/prompt/write_scope`
+ optional `verify_cmd`, `handoff_path`; `session_id == task_id`).

The call returns immediately with the wave snapshot (same shape as
`wave.status`). Spawning an already-running wave is idempotent and returns
the live snapshot.

### `wave.status`

```json
{"op": "wave.status", "params": {"wave_id": "w1"}}
```

Returns `{wave, tasks, summary}` where `summary` is
`{total, done, by_status, merge_gate}`. `merge_gate` is `completed` /
`failed` / `running` / `cancelled`.

### `wave.cancel`

```json
{"op": "wave.cancel", "params": {"wave_id": "w1"}}
```

Marks every non-terminal task `cancelled` (with `completed_at` and a reason
in `result`), DMs `kind=control {"action":"cancel","task_id":...}` to each
running task's worker per the §4.2 cancel contract, and finalizes the wave
as `cancelled`.

## Task lifecycle

```
pending ──► running ──► done | failed | cancelled
```

- **Dispatch.** When a task's dependencies are all `done`, the orchestrator
  marks it `running` (`started_at` set, persisted) and sends the
  `session_start` DM.
- **Completion.** A task completes when its assigned worker publishes a
  `completed` event (`data.result`) or a terminal `kind=message` result
  (`status: done`) on the wave/task channel. `data.task_id` or the channel
  suffix identifies the task.
- **Sender enforcement.** Only envelopes whose `meta.from` equals the task's
  assigned `worker` can change its state. Foreign senders are logged and
  ignored — nobody else can mark your task `completed`. (Full trust in
  `meta.from` lands with T1 identity binding.)
- **Failure is fail-fast.** The first `failed` task (or a task whose worker
  goes silent, or a wave timeout) cancels every remaining task and marks the
  wave `failed`.
- **Worker-reported cancellation.** A `status: "cancelled"` terminal message
  cancels that task; pending tasks whose dependencies can now never complete
  cascade to `cancelled`. When all tasks are terminal the wave finalizes via
  the merge gate.
- **verify_cmd.** When the task record has `verify_cmd`, the worker runs it
  and emits a `milestone {name: "verify_passed"}` event or an `error` event.
  The orchestrator records `verify_result` on the task: `"passed"` |
  `"failed"` | `"missing"` (task completed without the milestone).
- **Liveness.** The orchestrator tracks `hub.presence` heartbeats and any
  wave-channel envelope per worker identity. If a running task's worker has
  been silent longer than the TTL — `--wave-liveness-secs` flag or
  `NATS_HUB_WAVE_LIVENESS_SECS` env on hub-server, default **90s** — the task
  fails (fail-fast applies).
- **Restart resume.** On hub-server startup the orchestrator reloads every
  `running` wave from storage, seeds worker liveness from the agent registry,
  re-sends `session_start` for `running` tasks (at-least-once), and dispatches
  any newly-ready pending tasks. An orphaned wave whose tasks are all
  terminal is finalized immediately via the merge gate.

## Progress events

The orchestrator publishes `kind=event` envelopes on `channel.wave.<id>`
(existing subject space — `hub-watch --wave <id>` keeps working):

| event_type | data |
|---|---|
| `wave_started` | `wave_id`, `timeout_secs` |
| `task_dispatched` | `task_id`, `worker` |
| `task_completed` | `task_id`, `result` |
| `task_failed` | `task_id`, `error` |
| `task_cancelled` | `task_id`, optional `reason` |
| `wave_completed` / `wave_failed` / `wave_cancelled` | `wave_id`, `status`, optional `reason` |

## CLI

`hub-wave` is a thin client over the API — it never orchestrates:

```bash
hub-wave create --goal "Ship X" --from me --tasks tasks.json   # prints wave_id
hub-wave spawn <wave-id> --timeout 3600   # returns immediately
hub-wave status <wave-id>                 # wave + task table (incl. VERIFY)
hub-wave cancel <wave-id>
hub-wave list [--status running]
```

`hub-watch --wave <id>` still streams wave progress live.

## Session durability (worker hook)

Workers persist their backend-native session handle (claude session id,
codex thread id, cursor agent id, …) on the session record so a restarted
worker can resume a session it no longer holds in memory:

```json
{"op": "session.set_backend_ctx",
 "params": {"session_id": "t1", "backend_ctx": {"claude_session_id": "abc"}}}
```

`session.get` returns the stored value as `backend_ctx`. The T3 runtime is
expected to call `session.set_backend_ctx` whenever a backend session handle
appears or changes, and to read `session.get` on a `session_start` whose
session_id it doesn't hold in memory.
