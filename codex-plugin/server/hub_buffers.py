"""nats-hub MCP — subscriptions, buffers and the reply contract.

Owns every long-lived NATS subscription the server makes:

- ``channel.inbox.<identity>`` — DMs to this orchestrator (ring buffer).
- ``channel.task.<id>`` — per ``delegate_async``; progress + result tracking.
- ``channel.session.<id>`` — lazily per ``session_replies`` call.
- ``channel.wave.<id>(.>)`` — per ``spawn_wave``; task lifecycle tracking.

Result matching follows the shared reply contract (refocus.md §6): the
terminal result on a task channel is the first ``kind == "message"`` envelope
correlated to the task envelope id (``meta.reply_to`` or ``payload.task_id``);
``status``/``event`` envelopes are progress, never the result.
"""

from __future__ import annotations

import asyncio
import json
import sys
import time
import uuid
from collections import deque
from typing import Any, Callable

import hub_connection as conn


def _note_bad_envelope(where: str, err: Exception) -> None:
    print(f"[nats-hub] dropping undecodable envelope on {where}: {err}",
          file=sys.stderr)

INBOX_BUFFER_MAX = 500
EVENTS_PER_TASK = 50
CHANNEL_BUFFER_MAX = 200
SUB_READY_DELAY = 0.05  # let NATS register interest before publishing


def is_task_result(env: dict, task_id: str) -> bool:
    """Reply contract §6.5 — first kind=message correlated to the task id."""
    meta = env.get("meta", {})
    if meta.get("kind") != "message":
        return False
    payload = env.get("payload", {})
    return meta.get("reply_to") == task_id or payload.get("task_id") == task_id


class RingBuffer:
    """Bounded seq-numbered buffer of decoded envelopes."""

    def __init__(self, maxlen: int) -> None:
        self._items: deque[dict] = deque(maxlen=maxlen)
        self._seq = 0
        self._cond = asyncio.Condition()

    async def put(self, env: dict) -> None:
        async with self._cond:
            self._seq += 1
            self._items.append({"seq": self._seq, "env": env})
            self._cond.notify_all()

    def since(self, seq: int, limit: int | None = None) -> list[dict]:
        items = [it for it in self._items if it["seq"] > seq]
        if limit is not None:
            items = items[-limit:] if limit > 0 else []
        return items

    def tail(self, limit: int) -> list[dict]:
        return list(self._items)[-limit:] if limit > 0 else []

    @property
    def last_seq(self) -> int:
        return self._seq

    async def wait_for(
        self,
        predicate: Callable[[dict], bool],
        *,
        since: int = 0,
        timeout: float | None = None,
    ) -> dict | None:
        """First buffered/arriving item (seq > since) matching predicate."""
        deadline = None if timeout is None else time.monotonic() + timeout
        async with self._cond:
            while True:
                for it in self._items:
                    if it["seq"] > since and predicate(it["env"]):
                        return it
                if deadline is not None:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        return None
                    try:
                        await asyncio.wait_for(self._cond.wait(), remaining)
                    except asyncio.TimeoutError:
                        return None
                else:
                    await self._cond.wait()


class TaskTracker:
    """Tracks one delegated task's progress stream + terminal result."""

    def __init__(self, task_id: str, task_channel: str, worker: str) -> None:
        self.task_id = task_id
        self.task_channel = task_channel
        self.worker = worker
        self.events = RingBuffer(EVENTS_PER_TASK)
        self.other_messages = RingBuffer(100)
        self.last_status: str | None = None
        self.result: dict | None = None
        self.state = "running"  # running | done | error
        self.done = asyncio.Event()
        self.created_at = time.time()

    async def feed(self, env: dict) -> None:
        meta = env.get("meta", {})
        kind = meta.get("kind")
        if kind == "status":
            status = env.get("payload", {}).get("status")
            if isinstance(status, str):
                self.last_status = status
            await self.events.put(env)
        elif kind == "event":
            await self.events.put(env)
        elif is_task_result(env, self.task_id) and self.result is None:
            self.result = env
            payload = env.get("payload", {})
            self.state = "error" if payload.get("status") == "error" else "done"
            self.done.set()
        else:
            await self.other_messages.put(env)

    def snapshot(self, events_tail: int = 10) -> dict:
        snap: dict[str, Any] = {
            "task_id": self.task_id,
            "task_channel": self.task_channel,
            "worker": self.worker,
            "state": self.state,
            "last_status": self.last_status,
            "events": [
                {"seq": it["seq"], "from": it["env"]["meta"].get("from"),
                 "kind": it["env"]["meta"].get("kind"),
                 "payload": it["env"].get("payload")}
                for it in self.events.tail(events_tail)
            ],
        }
        if self.result is not None:
            snap["result"] = self.result.get("payload")
        return snap


class ChannelBuffer:
    """Bounded buffer + subscription for one ``channel.<name>``."""

    def __init__(self, subject: str, maxlen: int = CHANNEL_BUFFER_MAX) -> None:
        self.subject = subject
        self.buf = RingBuffer(maxlen)
        self._sub = None

    async def start(self) -> None:
        if self._sub is not None:
            return
        nc = await conn.get_nc()

        async def cb(msg) -> None:
            try:
                env = json.loads(msg.data.decode())
            except Exception as e:
                _note_bad_envelope(self.subject, e)
                return
            await self.buf.put(env)

        self._sub = await nc.subscribe(self.subject, cb=cb)
        await nc.flush()


class WaveTracker:
    """In-process wave spawn orchestration (spawn_via_api equivalent)."""

    def __init__(self, wave_id: str, tasks: list[dict]) -> None:
        self.wave_id = wave_id
        self.tasks: dict[str, dict] = {t["task_id"]: dict(t) for t in tasks}
        self.completed: set[str] = set()
        self.state = "running"  # running | completed | failed | timeout
        self.error: str | None = None
        self.done = asyncio.Event()
        self._bg: asyncio.Task | None = None


class HubState:
    """All subscriptions and in-flight coordination state."""

    def __init__(self) -> None:
        self.inbox = ChannelBuffer("channel.inbox.placeholder", INBOX_BUFFER_MAX)
        self.tasks: dict[str, TaskTracker] = {}
        self.sessions: dict[str, ChannelBuffer] = {}
        self.waves: dict[str, WaveTracker] = {}
        self._started = False
        self._lock = asyncio.Lock()

    async def ensure_started(self) -> None:
        """Connect + open the orchestrator inbox subscription once."""
        async with self._lock:
            nc = await conn.get_nc()  # also resolves identity (fails if unset)
            if self._started:
                return
            ident = conn.identity()
            self.inbox = ChannelBuffer(f"channel.inbox.{ident}", INBOX_BUFFER_MAX)
            await self.inbox.start()
            self._started = True

    async def delegate_async(self, worker: str, prompt: str) -> TaskTracker:
        """Subscribe to the task channel BEFORE DMing the worker (§6.1)."""
        await self.ensure_started()
        task_channel = f"task.{uuid.uuid4().hex[:8]}"
        env = conn.envelope(
            task_channel,
            {"prompt": prompt, "task_channel": task_channel},
            kind="message",
            to=worker,
        )
        task_id = env["meta"]["id"]
        tracker = TaskTracker(task_id, task_channel, worker)

        async def cb(msg) -> None:
            try:
                data = json.loads(msg.data.decode())
            except Exception as e:
                _note_bad_envelope(f"channel.{task_channel}", e)
                return
            await tracker.feed(data)

        nc = await conn.get_nc()
        await nc.subscribe(f"channel.{task_channel}", cb=cb)
        await nc.flush()
        await asyncio.sleep(SUB_READY_DELAY)

        self.tasks[task_id] = tracker
        await conn.publish(task_channel, env)
        return tracker

    async def task_status(self, task_id: str, events_tail: int = 10) -> dict | None:
        tracker = self.tasks.get(task_id)
        return tracker.snapshot(events_tail) if tracker else None

    async def wait_for_task(self, task_id: str, timeout: float) -> dict | None:
        tracker = self.tasks.get(task_id)
        if tracker is None:
            return None
        try:
            await asyncio.wait_for(tracker.done.wait(), timeout=timeout)
        except asyncio.TimeoutError:
            return {"state": "timeout", "task_id": task_id,
                    "last_status": tracker.last_status}
        return tracker.snapshot()

    async def session_buffer(self, session_id: str) -> ChannelBuffer:
        await self.ensure_started()
        ch = self.sessions.get(session_id)
        if ch is None:
            ch = ChannelBuffer(f"channel.session.{session_id}")
            self.sessions[session_id] = ch
            await ch.start()
        return ch

    async def spawn_wave(self, wave_id: str, timeout: float = 3600) -> WaveTracker:
        """Start the in-process spawn loop for a wave (see hub_wave.rs)."""
        await self.ensure_started()
        if wave_id in self.waves and self.waves[wave_id].state == "running":
            return self.waves[wave_id]
        resp = await conn.api_request("wave.list_tasks", {"wave_id": wave_id})
        if not resp.get("ok"):
            raise RuntimeError(resp.get("error", "wave.list_tasks failed"))
        tasks = resp.get("data", {}).get("tasks") or []
        if not tasks:
            raise RuntimeError(f"no tasks found for wave {wave_id!r}")
        tracker = WaveTracker(wave_id, tasks)
        self.waves[wave_id] = tracker
        tracker._bg = asyncio.create_task(self._run_wave(tracker, timeout))
        return tracker

    async def _run_wave(self, tracker: WaveTracker, timeout: float) -> None:
        wave_id = tracker.wave_id
        wave_prefix = f"wave.{wave_id}"
        events = ChannelBuffer(f"channel.{wave_prefix}.>")
        wave_ch = ChannelBuffer(f"channel.{wave_prefix}")
        try:
            await events.start()
            await wave_ch.start()
            await conn.api_request("wave.update_status",
                                   {"wave_id": wave_id, "status": "running"})
            deadline = time.monotonic() + timeout
            while tracker.state == "running":
                tmap = tracker.tasks
                if all(t["status"] in ("done", "failed") for t in tmap.values()):
                    any_failed = any(t["status"] == "failed" for t in tmap.values())
                    tracker.state = "failed" if any_failed else "completed"
                    break
                if time.monotonic() >= deadline:
                    tracker.state = "timeout"
                    break
                ready = [
                    t for t in tmap.values()
                    if t["status"] == "pending"
                    and all(d in tracker.completed
                            for d in (t.get("dependencies") or []))
                ]
                for task in ready:
                    await self._start_wave_task(tracker, task)
                    tmap[task["task_id"]]["status"] = "running"
                    await conn.api_request("wave.update_task_status", {
                        "wave_id": wave_id, "task_id": task["task_id"],
                        "status": "running",
                    })
                env = await self._next_wave_envelope(tracker, events, wave_ch)
                if env is not None:
                    self._handle_wave_event(tracker, env)
        except Exception as e:
            tracker.state = "failed"
            tracker.error = str(e)
        finally:
            final = {"completed": "completed", "failed": "failed",
                     "timeout": "failed"}.get(tracker.state)
            if final:
                await conn.api_request(
                    "wave.update_status",
                    {"wave_id": wave_id, "status": final})
            tracker.done.set()

    async def _next_wave_envelope(
        self, tracker: WaveTracker, events: ChannelBuffer, wave_ch: ChannelBuffer
    ) -> dict | None:
        """One envelope from the wave's channel buffers; ~1s tick when idle."""
        seqs = tracker.__dict__.setdefault("_buf_seqs", {})
        for ch in (events, wave_ch):
            items = ch.buf.since(seqs.get(ch.subject, 0), limit=1)
            if items:
                seqs[ch.subject] = items[0]["seq"]
                return items[0]["env"]
        it = await events.buf.wait_for(
            lambda _e: True, since=seqs.get(events.subject, 0), timeout=1.0)
        if it is not None:
            seqs[events.subject] = it["seq"]
            return it["env"]
        return None

    async def _start_wave_task(self, tracker: WaveTracker, task: dict) -> None:
        wave_id = tracker.wave_id
        task_id = task["task_id"]
        task_channel = f"wave.{wave_id}.task.{task_id}"
        payload: dict[str, Any] = {
            "action": "session_start", "session_id": task_id,
            "wave_id": wave_id, "channel": task_channel,
            "prompt": task["goal"], "write_scope": task.get("write_scope"),
        }
        if task.get("verify_cmd"):
            payload["verify_cmd"] = task["verify_cmd"]
        if task.get("handoff_path"):
            payload["handoff_path"] = task["handoff_path"]
        env = conn.envelope(task_channel, payload, kind="message",
                            to=task["worker"])
        await conn.publish(task_channel, env)

    def _handle_wave_event(self, tracker: WaveTracker, env: dict) -> None:
        if env.get("meta", {}).get("kind") != "event":
            return
        payload = env.get("payload", {})
        etype = payload.get("event_type")
        task_id = env["meta"].get("channel", "").rsplit(".task.", 1)[-1]
        if task_id == env["meta"].get("channel"):
            task_id = payload.get("data", {}).get("task_id") or payload.get("task_id")
        task = tracker.tasks.get(task_id)
        if task is None:
            return
        data = payload.get("data", {})
        if etype == "completed":
            self._finish_wave_task(tracker, task, "done",
                                  data.get("result", ""))
        elif etype == "error":
            self._finish_wave_task(tracker, task, "failed",
                                  data.get("error", "failed"))

    def _finish_wave_task(
        self, tracker: WaveTracker, task: dict, status: str, result: str
    ) -> None:
        task["status"] = status
        task["result"] = result
        if status == "done":
            tracker.completed.add(task["task_id"])
        asyncio.ensure_future(conn.api_request("wave.update_task_status", {
            "wave_id": tracker.wave_id, "task_id": task["task_id"],
            "status": status, "result": result,
        }))


_state: HubState | None = None


def hub() -> HubState:
    global _state
    if _state is None:
        _state = HubState()
    return _state
