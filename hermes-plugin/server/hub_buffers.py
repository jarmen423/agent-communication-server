"""nats-hub MCP — subscriptions, buffers and the reply contract.

Owns every long-lived NATS subscription the server makes:

- ``channel.inbox.<identity>`` — DMs to this orchestrator (ring buffer).
- ``channel.task.<id>`` — per ``delegate_async``; progress + result tracking.
- ``channel.session.<id>`` — lazily per ``session_replies`` call.

Result matching follows the shared reply contract (refocus.md §6): the
terminal result on a task channel is the first ``kind == "message"`` envelope
correlated to the task envelope id (``meta.reply_to`` or ``payload.task_id``);
``status``/``event`` envelopes are progress, never the result.
"""

from __future__ import annotations

import asyncio
import json
import sys
import uuid

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
)


class HubState:
    """All subscriptions and in-flight coordination state."""

    def __init__(self) -> None:
        self.inbox = ChannelBuffer("channel.inbox.placeholder", INBOX_BUFFER_MAX)
        self.tasks: dict[str, TaskTracker] = {}
        self.sessions: dict[str, ChannelBuffer] = {}
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


_state: HubState | None = None


def hub() -> HubState:
    global _state
    if _state is None:
        _state = HubState()
    return _state
