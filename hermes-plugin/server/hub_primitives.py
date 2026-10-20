"""nats-hub MCP — buffer/tracker primitives and the reply-contract matcher.

Split out of ``hub_buffers`` (which owns the long-lived subscriptions).
Result matching follows the shared reply contract (refocus.md §6): the
terminal result on a task channel is the first ``kind == "message"`` envelope
correlated to the task envelope id (``meta.reply_to`` or ``payload.task_id``);
``status``/``event`` envelopes are progress, never the result. A result
counts only when ``meta.from`` is the worker the task was delegated to
(HARDENING.md C5; best effort on a permissive hub, where ``meta.from`` is
self-asserted, real on a ``--require-bound-identity`` hub).
"""

from __future__ import annotations

import asyncio
import json
import sys
import time
from collections import deque
from typing import Any, Callable

import hub_connection as conn
import hub_delivery


def _note_bad_envelope(where: str, err: Exception) -> None:
    print(f"[nats-hub] dropping undecodable envelope on {where}: {err}",
          file=sys.stderr)

INBOX_BUFFER_MAX = 500
MAX_TRACKED_TASKS = 200  # oldest finished trackers are evicted first
TASK_UNSUB_GRACE = 5.0  # keep listening briefly after the result (late events)
EVENTS_PER_TASK = 50
CHANNEL_BUFFER_MAX = 200
SUB_READY_DELAY = 0.05  # let NATS register interest before publishing


def is_task_result(env: dict, task_id: str, worker: str | None = None) -> bool:
    """Reply contract §6.5 — first kind=message correlated to the task id,
    and (C5) sent by ``worker`` when given."""
    meta = env.get("meta", {})
    if meta.get("kind") != "message":
        return False
    if worker is not None and meta.get("from") != worker:
        return False
    payload = env.get("payload", {})
    return meta.get("reply_to") == task_id or payload.get("task_id") == task_id


def summarize_env(it: dict) -> dict:
    """A buffered item (``{seq, env}``) as a flat dict for tool output."""
    env = it["env"]
    return {
        "seq": it["seq"],
        "id": env.get("meta", {}).get("id"),
        "from": env.get("meta", {}).get("from"),
        "to": env.get("meta", {}).get("to"),
        "channel": env.get("meta", {}).get("channel"),
        "kind": env.get("meta", {}).get("kind"),
        "timestamp": env.get("meta", {}).get("timestamp"),
        "reply_to": env.get("meta", {}).get("reply_to"),
        "payload": env.get("payload"),
    }


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
        self.state = "running"  # running | done | error | cancelled | evicted
        self.done = asyncio.Event()
        self.created_at = time.time()
        self.cancel_requested_at: float | None = None  # set by cancel_task
        self.sub = None  # NATS subscription on channel.<task_channel>
        # Reads the stored task channel back (durable hub) and dedupes it
        # against the live subscription: at-least-once → handled once.
        self.catch_up = hub_delivery.ChannelCatchUp(task_channel)

    async def close(self) -> None:
        if self.sub is not None:
            sub, self.sub = self.sub, None
            try:
                await sub.unsubscribe()
            except Exception:
                pass  # connection already closed — nothing to release

    async def feed(self, env: dict) -> None:
        if not self.catch_up.first_sight(env):
            return  # redelivery, or already read back from the stream
        await self._feed(env)

    async def catch_up_now(self) -> None:
        """Feed whatever the stored task channel has that we missed (e.g.
        published while the connection was down). No-op on a core hub."""
        if self.done.is_set() or self.catch_up.available is False:
            return
        try:
            envs = await self.catch_up.poll(await conn.get_nc())
        except Exception as e:  # noqa: BLE001 - best effort; live sub still runs
            print(f"[nats-hub] task catch-up failed: {e}", file=sys.stderr)
            return
        for env in envs:
            await self._feed(env)

    async def _feed(self, env: dict) -> None:
        meta = env.get("meta", {})
        kind = meta.get("kind")
        if kind == "status":
            status = env.get("payload", {}).get("status")
            if isinstance(status, str):
                self.last_status = status
            await self.events.put(env)
        elif kind == "event":
            await self.events.put(env)
        elif is_task_result(env, self.task_id, self.worker) and self.result is None:
            self.result = env
            payload = env.get("payload", {})
            status = payload.get("status")
            # Terminal status set: done | error | cancelled (contract §4.2).
            self.state = status if status in ("error", "cancelled") else "done"
            self.done.set()
        else:
            if is_task_result(env, self.task_id):  # right task, wrong sender
                print(f"[nats-hub] ignoring result for {self.task_id} from "
                      f"{env.get('meta', {}).get('from')!r} (delegated to "
                      f"{self.worker!r})", file=sys.stderr)
            await self.other_messages.put(env)

    def fail_connection_lost(self) -> None:
        """The connection was replaced and the result can't be recovered."""
        if not self.done.is_set():
            self.state = "error"
            self.last_status = "connection lost; task result unknown"
            self.done.set()

    def evict(self) -> None:
        """Dropped from tracking while running: release waiters instead of
        letting them time out (the worker may still finish; see history)."""
        if not self.done.is_set():
            self.state = "evicted"
            self.last_status = "evicted: too many tracked tasks"
            self.done.set()

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
        self._seen = hub_delivery.SeenIds()

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
            await self.put(env)

        self._sub = await nc.subscribe(self.subject, cb=cb)
        await nc.flush()

    async def put(self, env: dict) -> None:
        """Buffer ``env`` unless its ``meta.id`` was already buffered."""
        meta = env.get("meta") if isinstance(env, dict) else None
        if self._seen.first_sight(meta.get("id") if isinstance(meta, dict) else None):
            await self.buf.put(env)

    async def stop(self) -> None:
        if self._sub is not None:
            sub, self._sub = self._sub, None
            try:
                await sub.unsubscribe()
            except Exception:
                pass




class InboxBuffer(ChannelBuffer):
    """The orchestrator's inbox: the durable JetStream inbox on a
    ``--durable`` hub (DMs sent while this MCP server was down are buffered
    first), else a core subscription. Redeliveries are dropped by ``meta.id``."""

    def __init__(self, identity: str, maxlen: int = INBOX_BUFFER_MAX) -> None:
        super().__init__(f"channel.inbox.{identity}", maxlen)
        self.identity = identity
        self.durable = False

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
            await self.put(env)

        self._sub = await hub_delivery.subscribe_inbox(nc, self.identity, cb)
        self.durable = self._sub.durable
        await nc.flush()
