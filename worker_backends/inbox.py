"""Inbox dispatch for the Python worker runtime (cancel contract §4.2).

nats-py runs a subscription's callback for one message at a time, so a
callback that awaits a whole task would also hold back a cancel for it. The
inbox callback here never blocks:

- a cancel DM (``kind=control {"action": "cancel", "task_id"}``) is applied
  at once: a running turn is cancelled through its ``TurnCancel`` handle; a
  queued delegated task (``payload.task_channel``) gets its ``cancelled``
  result immediately and is skipped later; other queued work is marked and
  reports ``cancelled`` when reached (it never runs the backend);
- everything else is queued and handled one at a time by :meth:`run`, in
  arrival order (the runtime's existing one-task-at-a-time semantics).
"""

from __future__ import annotations

import asyncio
import json
import logging
from typing import Any, Awaitable, Callable

from worker_backends.task_registry import TaskRegistry, cancel_request

logger = logging.getLogger("worker_runtime")

NON_TASK_KINDS = ("status", "event", "control")

Handler = Callable[[Any, dict[str, Any]], Awaitable[None]]  # (msg, envelope)
Reporter = Callable[[dict[str, Any]], Awaitable[None]]  # (queued envelope)


class InboxDispatcher:
    def __init__(self, inbox_subject: str, registry: TaskRegistry, handle: Handler,
                 report_cancelled: Reporter, log: str = "worker") -> None:
        self.inbox_subject = inbox_subject
        self.registry = registry
        self.handle = handle
        self.report_cancelled = report_cancelled
        self.log = log
        self._queue: asyncio.Queue[tuple[Any, dict[str, Any]]] = asyncio.Queue()
        self._queued: dict[str, dict[str, Any]] = {}  # id → envelope, not started
        self._reported: set[str] = set()  # queued tasks already answered

    async def on_message(self, msg: Any) -> None:
        try:
            envelope = json.loads(msg.data.decode())
        except Exception as e:  # noqa: BLE001
            print(f"[{self.log}] decode error: {e}")
            return
        meta = envelope.get("meta") or {}
        cancel_id = cancel_request(envelope)
        if cancel_id is not None:
            if msg.subject == self.inbox_subject:  # cancels are DMs, not chatter
                await self._cancel(cancel_id, meta.get("from"))
            return
        task_id = meta.get("id")
        if meta.get("kind") not in NON_TASK_KINDS and task_id:
            self.registry.register(task_id)  # cancellable while queued
            self._queued[task_id] = envelope
        self._queue.put_nowait((msg, envelope))

    async def _cancel(self, task_id: str, by: str | None) -> None:
        if not self.registry.cancel(task_id, by=by):
            logger.debug("cancel for unknown/finished task %s ignored", task_id)
            return
        print(f"[{self.log}] cancel {task_id} (requested by {by})")
        queued = self._queued.pop(task_id, None)
        if queued and (queued.get("payload") or {}).get("task_channel"):
            self._reported.add(task_id)
            await self.report_cancelled(queued)

    async def run(self) -> None:
        while True:
            msg, envelope = await self._queue.get()
            task_id = (envelope.get("meta") or {}).get("id")
            self._queued.pop(task_id, None)
            try:
                if task_id in self._reported:
                    self._reported.discard(task_id)
                    continue
                await self.handle(msg, envelope)
            except Exception as e:  # noqa: BLE001 - one bad task must not stop the loop
                print(f"[{self.log}] handler error: {e}")
            finally:
                self.registry.finish(task_id)
