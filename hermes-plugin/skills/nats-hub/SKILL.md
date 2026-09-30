---
name: nats-hub
description: Coordinate with other AI agents via the nats-hub messaging bus. Use this skill when you need to check which workers are usable, delegate or cancel tasks, read replies, follow multi-turn sessions, or orchestrate parallel work across multiple agents.
---

# NATS Hub: Orchestrating Agents on the Bus

The nats-hub MCP tools connect you to a NATS-based bus shared by other agents
and humans.

## Your identity is bound, not chosen

- Every message you send is stamped with **your identity** as `meta.from`.
  It comes from the environment, never from tool arguments: `NATS_HUB_IDENTITY`
  if the host sets it, otherwise the plugin's per-host default
  (`claude-code-agent`, `codex-agent`, `hermes-agent`).
- **No tool takes a `from` argument.** Passing one that doesn't match is an
  error. `from` appears only as a *filter* (`wait_for_message`, `get_history`).
- `whoami` shows your identity, where it came from, and the NATS URL.
- On a hub with per-agent credentials, the hub also enforces this identity:
  you can't publish as someone else or read another agent's inbox.

## 1. Pick a worker: `check_providers`

```
check_providers()                          → alive workers: capabilities, provider, models
check_providers(ping=true)                 → + a tiny real task to each worker, in parallel
check_providers(ping=true, workers=["claude-1"], ping_timeout=20)
```

- Each worker's `ping.status` is one of:
  - `ok`: answered within `slow_after` (10s by default).
  - `slow`: answered, but late.
  - `error`: the worker answered, but its backend failed, for example auth or
    out of credits. `detail` says why.
  - `unresponsive`: no answer within `ping_timeout`. The ping is cancelled.
  - `not_pinged`: not a `worker`-capability agent, or not in `workers`.
- The call is bounded by `ping_timeout` and never hangs.
- `models` and `model_source` show where the model came from: registry
  metadata, the worker supervisor, or `model:`/`provider:` capabilities. A
  worker that advertises nothing runs its CLI's default model.
- **What it can't tell you:** remaining API credits or rate limits. A ping
  proves only that one tiny request worked just now. Read `does_not_verify` in
  the result. Pinging an LLM worker costs one small request.
- `list_agents(alive_within_secs=120)` is the cheap roster, with no ping.

## 2. Delegate, watch, collect

Delegation is async end to end. The server buffers the task channel for you,
so don't poll the database.

```
delegate_async(to="echo-1", prompt="summarize the log at /tmp/x.log")
  → {task_id, task_channel}            # returns immediately

wait_for_task(task_id, timeout=120)
  → {state: "done", result: {status, result, error, task_id}, events, last_status}
```

- **`delegate_async`** subscribes to the task channel *before* DMing the
  worker, so nothing is missed.
- **`wait_for_task`** blocks until the worker's *terminal result*, not a
  `started` or `progress` event. On timeout it returns an error with the last
  status. Call it again to keep waiting.
- **`task_status`** is the non-blocking snapshot. `state` is
  `running | done | error | cancelled`. It also has `last_status`, recent
  `events`, and `result` once the task has finished.
- **`delegate_task`** is `delegate_async` plus `wait_for_task` in one call.

The terminal result payload is
`{"status": "done"|"error"|"cancelled", "task_id", "result", "error"}`.

## 3. Stop a task: `cancel_task`

```
cancel_task(task_id)                  → terminal snapshot, state "cancelled"
cancel_task(task_id, timeout=30)      → wait longer for the worker to confirm
```

- It DMs the worker `{action: "cancel", task_id}`. The worker kills the
  running backend and publishes a result with `status: "cancelled"`.
- If the task already finished, you get its final snapshot back, with a
  `note`, and nothing is sent.
- If the worker doesn't confirm in time (an older worker without cancel
  support, or a busy one), you get an **error** with the current snapshot.
  The task isn't marked cancelled locally, because it may still finish. Call
  `cancel_task` again, or `wait_for_task`.
- You can only cancel tasks that this MCP server delegated.

## 4. Your inbox

Workers and humans DM you on `channel.inbox.<your identity>`. The server
subscribes at connect time and keeps a bounded buffer.

```
read_inbox(limit=20)                  → recent DMs, each with a seq, plus last_seq
wait_for_message(timeout=60)          → the next NEW DM (arriving after this call)
wait_for_message(timeout=60, from="echo-1")
wait_for_message(timeout=60, since_seq=<last_seq>)
                                      → also match DMs already buffered after last_seq
```

- `wait_for_message` returns **only new messages** by default. A message that
  arrived before the call does not satisfy it. To catch up, use `read_inbox`,
  or pass `since_seq` to replay from a point you've already read.
- Use `since_seq` (from `last_seq`) to read only what's new, without re-reading
  or missing anything.

## 5. Multi-turn work: sessions

```
start_session(worker="claude-1", prompt="Help me refactor foo.rs")
  → {session_id, channel}
session_replies(session_id)           → the worker's responses (seq'd)
send_to_session(session_id, "Now add tests")
close_session(session_id)
```

`start_session` opens the reply buffer before the worker can answer, so the
transcript is always readable from seq 1.

## 6. Parallel work: waves

Use a wave for **several workers on disjoint write scopes**, with optional
dependencies. For one or two tasks, `delegate_async` is simpler.

```
create_wave(goal="Split auth module", tasks=[
  {task_id: "a", worker: "claude-1", goal: "Refactor auth-rs", write_scope: ["src/auth/**"]},
  {task_id: "b", worker: "codex-1",  goal: "Update auth-api", write_scope: ["api/**"], dependencies: ["a"]},
])
  → {wave_id, tasks: [...]}
spawn_wave(wave_id)                   → dispatches ready tasks, honors deps
list_wave_tasks(wave_id) / get_wave(wave_id)   → watch it
```

Read the `note` in `spawn_wave`'s result. If the wave is driven from this MCP
server, keep your session alive until the wave finishes.

## Everything else

- `send_message` broadcasts to a channel, `send_direct` DMs one agent, and
  `send_status` posts a status.
- `get_history`, `get_thread` and `list_pending` query the persisted store.
  Use them for tasks from before this server started.
- `get_analytics` returns message rate, latency, hotspots and error rate.

## Errors tell you what to do next

- Every tool returns `{"ok": false, "error": ...}` instead of hanging.
- Bad arguments name the field and list the expected ones. Unknown arguments
  are rejected, so typos don't pass silently.
- If the bus is down, the error says `cannot reach NATS at <url>`. If
  hub-server is down, it says `hub-server is not answering`.
- In the agent-communication-server repo, `make up` starts nats-server,
  hub-server and two echo workers (`echo-1`, `echo-2`) you can delegate to.
