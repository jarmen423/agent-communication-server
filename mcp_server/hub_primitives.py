"""nats-hub MCP — buffer/tracker primitives and the reply-contract matcher.

Split out of ``hub_buffers`` (which owns the long-lived subscriptions).
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
from collections import deque
from typing import Any, Callable

import hub_connection as conn


def _note_bad_envelope(where: str, err: Exception) -> None:
    print(f"[nats-hub] dropping undecodable envelope on {where}: {err}",
          file=sys.stderr)

INBOX_BUFFER_MAX = 500
MAX_TRACKED_TASKS = 200  # oldest finished trackers are evicted first
TASK_UNSUB_GRACE = 5.0  # keep listening briefly after the result (late events)
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
        self.state = "running"  # running | done | error | cancelled
        self.done = asyncio.Event()
        self.created_at = time.time()
        self.cancel_requested_at: float | None = None  # set by cancel_task
        self.sub = None  # NATS subscription on channel.<task_channel>

    async def close(self) -> None:
        if self.sub is not None:
            sub, self.sub = self.sub, None
            try:
                await sub.unsubscribe()
            except Exception:
                pass  # connection already closed — nothing to release

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
            status = payload.get("status")
            # Terminal status set: done | error | cancelled (contract §4.2).
            self.state = status if status in ("error", "cancelled") else "done"
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
            "cancel_requested": self.cancel_requested_at is not None,
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

    async def stop(self) -> None:
        if self._sub is not None:
            sub, self._sub = self._sub, None
            try:
                await sub.unsubscribe()
            except Exception:
                pass


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
