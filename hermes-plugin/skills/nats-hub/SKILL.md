---
name: nats-hub
description: Coordinate with other AI agents via the nats-hub messaging bus. Use this skill when you need to send messages to other agents, delegate tasks, follow multi-turn sessions, or orchestrate parallel work across multiple agents.
---

# NATS Hub — Orchestrating Agents on the Bus

The nats-hub MCP tools connect you to a NATS-based bus shared by other agents
and humans. Your identity is the `NATS_HUB_IDENTITY` env var the plugin sets —
it is stamped on every message you send, so **no tool takes a `from` arg**.

## The core loop: delegate → watch → collect

Delegating a task is async end-to-end. Don't poll the DB; the server buffers
the task channel for you.

```
delegate_async(to="echo-1", prompt="summarize the log at /tmp/x.log")
  → {task_id, task_channel}            # returns immediately

wait_for_task(task_id, timeout=120)
  → {state: "done", result: {result: "...", task_id}, events, last_status}
```

- **`delegate_async`** subscribes to the task channel *before* DMing the
  worker, so nothing is missed. It returns `{task_id, task_channel}`.
- **`wait_for_task`** blocks until the worker's *terminal result* — the first
  `kind=message` envelope correlated to the task — not a `started`/`progress`
  event. On timeout it returns the last known status so you can keep waiting.
- **`task_status`** is the non-blocking snapshot: `state`
  (running/done/error), `last_status`, recent `events`, and `result` once done.
- **`delegate_task`** is the blocking convenience wrapper: same as
  `delegate_async` + `wait_for_task` for when you just want the answer.

A worker's terminal result payload is `{"status": "done"|"error",
"task_id": <id>, "result": <string|null>, "error": <string|null>}`.

## Follow up: your inbox

Workers DM you back on `channel.inbox.<your identity>` — the server subscribes
at connect time and keeps a bounded buffer.

```
read_inbox(limit=20)                  → recent DMs, each with a seq
wait_for_message(timeout=60)          → block for the next DM
wait_for_message(timeout=60, from="echo-1", since_seq=<last>)
                                      → block for a specific sender / newer only
```

Use `since_seq` (returned as `last_seq`) to read only what arrived since your
last call — no re-reading, no missing.

## Multi-turn work: sessions

For a conversation instead of a one-shot task:

```
start_session(worker="claude-1", prompt="Help me refactor foo.rs")
  → {session_id, channel}
session_replies(session_id)           → worker's responses (seq'd)
send_to_session(session_id, "Now add tests")
close_session(session_id)
```

The server lazily subscribes to `channel.session.<id>` the first time you call
`session_replies`, and `start_session` opens the buffer before the worker can
reply — you can always read the transcript from seq 1.

## Parallel work: waves

Use a wave when you want **several workers on independent, disjoint write
scopes** with optional dependencies — e.g. three modules refactored at once,
tests depending on the refactor task finishing. For one or two tasks,
`delegate_async` is simpler.

```
create_wave(goal="Split auth module", tasks=[
  {task_id: "a", worker: "claude-1", goal: "Refactor auth-rs", write_scope: ["src/auth/**"]},
  {task_id: "b", worker: "codex-1",  goal: "Update auth-api", write_scope: ["api/**"], dependencies: ["a"]},
])
  → {wave_id, tasks: [...]}
spawn_wave(wave_id)                   → dispatches ready tasks, honors deps,
                                       drives the wave to completed|failed
list_wave_tasks(wave_id) / get_wave(wave_id)   → watch it
```

`spawn_wave` runs the orchestration loop inside this MCP server — the session
must stay alive until the wave finishes.

## Picking a worker

```
list_agents(alive_within_secs=120)    → who's alive + capabilities
check_providers(providers=["claude"]) → alive agents + live model-list probes
```

`check_providers` is honest about scope: it verifies bus registration and
supervisor responses, not provider credentials.

## Everything else

- `send_message` — broadcast to a channel; `send_direct` — DM one agent;
  `send_status` — post a status.
- `get_history`, `get_thread`, `list_pending` — cold-path queries against the
  persisted store.
- `get_analytics` — message rate, latency, hotspots, error rate.

## If the bus is down

All tools return `{"ok": false, "error": ...}` — never a hang. In the
agent-communication-server repo, `make up` starts a local nats-server +
hub-server + two echo workers (`echo-1`, `echo-2`) you can delegate to.
