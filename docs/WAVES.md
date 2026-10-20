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
create time; repeated dependencies like `["root","root"]` count once).

Wave and task ids must each be **one NATS token**: non-empty, at most 128
bytes, printable ASCII without whitespace, `.`, `*`, `>`, and without `--`
(the separator in task session ids, see below). A wave that is already
`running` or terminal can't be re-created. `tasks` may be omitted for a wave-only create; the legacy form
(bare `WaveRecord` as `params`) still works.

### `wave.spawn`

```json
{"op": "wave.spawn", "params": {"wave_id": "w1", "timeout_secs": 3600}}
```

Hands the wave to the orchestrator: marks it `running`, records the timeout
in `wave.metadata.timeout_secs`, and DMs `session_start` to the worker of
every ready task on its task channel `wave.<id>.task.<task_id>`.

`timeout_secs` defaults to 3600 and is **clamped to 1 s … 7 days**
(604800); a non-integer or negative value is rejected. A stored timeout is
clamped again when a wave is resumed after a restart.

The `session_start` payload:

| field | value |
|---|---|
| `action` | `session_start` |
| `session_id` | `<wave_id>--<task_id>`: unique per wave (no session state carries over between waves), stable across re-dispatches |
| `wave_id`, `wave_task_id` | the wave and task ids |
| `channel`, `task_channel` | `wave.<wave_id>.task.<task_id>`: where the worker publishes progress and its terminal result (`task_channel` is what `hub-worker --execute` uses) |
| `prompt`, `write_scope` | the task's goal and scope |
| `verify_cmd`, `handoff_path` | optional |

The orchestrator remembers the **envelope id** of the latest `session_start`
per task (in memory and in `wave.metadata.dispatch_ids`); workers register
the turn under it, and cancels use it (below).

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
in `result`), DMs `kind=control {"action":"cancel","task_id": <session_start
envelope id>, "wave_task_id", "session_id"}` to each running task's worker per
the §4.2 cancel contract, then `session_close`, and finalizes the wave as
`cancelled`. Every DM (`session_start`, cancel, `session_close`) is sent as
`hub-orchestrator`, so workers that only accept session messages from the
session's starter accept them.

A terminal wave (`completed`, `failed`, `cancelled`) is final: `wave.cancel`
on it fails and changes nothing.

### Record ops and terminal states

- `wave.update_task_status` on a **running** wave goes through the
  orchestrator's state machine: only a `running` task can move, to `done`,
  `failed` (fail-fast; the worker gets a cancel) or `cancelled` (the worker
  gets a cancel), with the same effects as the worker's own result. On a
  pending wave it writes storage directly. A terminal task never changes
  (re-setting the same status is a no-op).
- `wave.update_status` can't change a running wave (use `wave.cancel`), a
  terminal wave, or start one (use `wave.spawn`).
- `wave.create_task` only adds to a pending wave and never overwrites a
  running or terminal task.

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
  suffix identifies the task. Results for a task that isn't `running`
  (never dispatched, or already terminal) are ignored. When a task ends the
  orchestrator DMs its worker `session_close` and closes the session record.
- **Sender enforcement.** Only envelopes whose `meta.from` equals the task's
  assigned `worker` can change its state. Foreign senders are logged and
  ignored — nobody else can mark your task `completed`. (Full trust in
  `meta.from` lands with T1 identity binding.)
- **Failure is fail-fast.** The first `failed` task (or a task whose worker
  goes silent, or a wave timeout) cancels every remaining task and marks the
  wave `failed`.
- **Worker-reported cancellation.** A `status: "cancelled"` terminal message
  (or an `error` event with `cancelled: true`) cancels that task; pending tasks whose dependencies can now never complete
  cascade to `cancelled`. When all tasks are terminal the wave finalizes via
  the merge gate.
- **verify_cmd.** When the task record has `verify_cmd`, the worker runs it
  and emits a `milestone {name: "verify_passed"}` event or an `error` event.
  The orchestrator records `verify_result` on the task: `"passed"` |
  `"failed"` | `"missing"` (task completed without the milestone).
- **Liveness.** The orchestrator tracks `hub.presence.<identity>` heartbeats
  and wave-channel envelopes from the workers of live waves (from no one
  else; entries for identities no live wave references are pruned). The
  legacy bare `hub.presence` (identity taken from the payload) counts only
  when hub-server runs without `--require-bound-identity`. If a running task's worker has
  been silent longer than the TTL — `--wave-liveness-secs` flag or
  `NATS_HUB_WAVE_LIVENESS_SECS` env on hub-server, default **90s** — the task
  fails (fail-fast applies).
- **Restart resume.** On hub-server startup the orchestrator first waits
  until the router routes (it DMs itself a probe until it arrives, up to
  15 s), then reloads every `running` wave from storage, seeds worker
  liveness from the agent registry, re-sends `session_start` for `running`
  tasks (at-least-once, same `session_id`; a worker still running that turn
  aliases the new envelope id instead of starting a second run), and
  dispatches any newly-ready pending tasks. An orphaned wave whose tasks are all
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

`session.get` returns the stored value as `backend_ctx`. The op updates only
that field, so it never overwrites a concurrent status change.

Wave tasks get a session record too: the orchestrator creates
`<wave_id>--<task_id>` at dispatch (`orchestrator: "hub-orchestrator"`,
`worker`: the task's worker, `metadata: {kind: "wave_task", wave_id, task_id,
wave_owner}`) and closes it when the task ends, so `session.set_backend_ctx`
and resume work for wave tasks. The record's `orchestrator` is the identity
that sends the session DMs, because workers take a resumed session's owner
from it. The T3 runtime is
expected to call `session.set_backend_ctx` whenever a backend session handle
appears or changes, and to read `session.get` on a `session_start` whose
session_id it doesn't hold in memory.
