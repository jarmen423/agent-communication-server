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
from typing import Any

import hub_connection as conn
from hub_primitives import (  # noqa: F401  (re-exported for callers/tests)
    _note_bad_envelope,
    INBOX_BUFFER_MAX,
    MAX_TRACKED_TASKS,
    TASK_UNSUB_GRACE,
    EVENTS_PER_TASK,
    CHANNEL_BUFFER_MAX,
    SUB_READY_DELAY,
    is_task_result,
    RingBuffer,
    TaskTracker,
    ChannelBuffer,
    WaveTracker,
)


class HubState:
    """All subscriptions and in-flight coordination state."""

    def __init__(self) -> None:
        self.inbox = ChannelBuffer("channel.inbox.placeholder", INBOX_BUFFER_MAX)
        self.tasks: dict[str, TaskTracker] = {}
        self.sessions: dict[str, ChannelBuffer] = {}
        self.waves: dict[str, WaveTracker] = {}
        self._started = False
        self._generation = 0
        self._lock = asyncio.Lock()
        self._bg: set[asyncio.Task] = set()

    def spawn_bg(self, coro) -> asyncio.Task:
        """Fire-and-forget with a strong reference and logged failures."""
        task = asyncio.ensure_future(coro)
        self._bg.add(task)

        def _done(t: asyncio.Task) -> None:
            self._bg.discard(t)
            if not t.cancelled() and t.exception() is not None:
                print(f"[nats-hub] background task failed: {t.exception()}",
                      file=sys.stderr)

        task.add_done_callback(_done)
        return task

    async def ensure_started(self) -> None:
        """Connect + open the orchestrator inbox subscription once (and again
        if the connection was replaced, since the old subscriptions died)."""
        async with self._lock:
            nc = await conn.get_nc()  # also resolves identity (fails if unset)
            if self._started and self._generation == conn.generation():
                return
            if self._started:
                self._reset_after_reconnect()
            self._generation = conn.generation()
            ident = conn.identity()
            self.inbox = ChannelBuffer(f"channel.inbox.{ident}", INBOX_BUFFER_MAX)
            await self.inbox.start()
            self._started = True

    def _reset_after_reconnect(self) -> None:
        """The connection was closed and replaced: every subscription is gone.
        Fail running tasks loudly instead of letting callers time out."""
        for t in self.tasks.values():
            t.sub = None
            if t.result is None:
                t.state = "error"
                t.last_status = "connection lost; task result unknown"
                t.done.set()
        self.sessions.clear()  # recreated lazily by session_buffer()

    async def _track(self, tracker: TaskTracker) -> None:
        """Register a tracker, evicting the oldest (finished first) over cap."""
        self.tasks[tracker.task_id] = tracker
        overflow = len(self.tasks) - MAX_TRACKED_TASKS
        if overflow <= 0:
            return
        by_age = sorted(self.tasks.values(), key=lambda t: t.created_at)
        victims = [t for t in by_age if t.done.is_set()][:overflow]
        if len(victims) < overflow:
            running = [t for t in by_age if not t.done.is_set()
                       and t is not tracker]
            victims += running[: overflow - len(victims)]
        for t in victims:
            self.tasks.pop(t.task_id, None)
            await t.close()

    async def drop_session(self, session_id: str) -> None:
        ch = self.sessions.pop(session_id, None)
        if ch is not None:
            await ch.stop()

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
            was_done = tracker.done.is_set()
            await tracker.feed(data)
            if tracker.done.is_set() and not was_done:
                # Result is in: release the subscription after a short grace
                # period (late progress events are harmless to drop).
                async def _release() -> None:
                    await asyncio.sleep(TASK_UNSUB_GRACE)
                    await tracker.close()

                self.spawn_bg(_release())

        nc = await conn.get_nc()
        tracker.sub = await nc.subscribe(f"channel.{task_channel}", cb=cb)
        await nc.flush()
        await asyncio.sleep(SUB_READY_DELAY)

        await self._track(tracker)
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
                failed = [t["task_id"] for t in tmap.values()
                          if t["status"] == "failed"]
                if failed:
                    # Mirror hub_wave.rs: abort on the first failed task —
                    # dependents would otherwise sit pending until timeout.
                    tracker.state = "failed"
                    tracker.error = f"task(s) failed: {', '.join(failed)}"
                    break
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
            await events.stop()
            await wave_ch.stop()
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
        self.spawn_bg(conn.api_request("wave.update_task_status", {
            "wave_id": tracker.wave_id, "task_id": task["task_id"],
            "status": status, "result": result,
        }))


_state: HubState | None = None


def hub() -> HubState:
    global _state
    if _state is None:
        _state = HubState()
    return _state
