"""Inbox dispatch for the Python worker runtime (cancel contract §4.2).

nats-py runs a subscription's callback for one message at a time, so a
callback that awaits a whole task would also hold back a cancel for it. The
inbox callback here never blocks:

- a cancel DM (``kind=control {"action": "cancel", "task_id"}``) is applied
  at once: a running turn is cancelled through its ``TurnCancel`` handle; a
  queued delegated task (``payload.task_channel``) gets its ``cancelled``
  result immediately and is skipped later; other queued work is marked and
  reports ``cancelled`` when reached (it never runs the backend). A cancel is
  honoured only from the sender of the task it names (HARDENING.md C5);
- everything else is queued and handled one at a time by :meth:`run`, in
  arrival order (the runtime's existing one-task-at-a-time semantics);
- session-channel traffic (``session_send``/``session_close`` broadcast on
  ``channel.session.<id>``) enters the same queue via
  :meth:`on_session_message`, so a queued follow-up turn is registered (and
  cancellable) the moment it arrives;
- a ``session_start`` for a session whose turn is still queued or running is
  not run twice (C3): its envelope id becomes an alias of that turn.
"""

from __future__ import annotations

import asyncio
import json
import logging
from typing import Any, Awaitable, Callable

from worker_backends.task_registry import TaskRegistry, cancel_request

logger = logging.getLogger("worker_runtime")

NON_TASK_KINDS = ("status", "event", "control")
SESSION_CHANNEL_ACTIONS = ("session_send", "session_close")

Handler = Callable[[Any, dict[str, Any]], Awaitable[None]]  # (msg, envelope)
Reporter = Callable[[dict[str, Any]], Awaitable[None]]  # (queued envelope)


def _decode(msg: Any, log: str) -> dict[str, Any] | None:
    try:
        envelope = json.loads(msg.data.decode())
    except Exception as e:  # noqa: BLE001
        print(f"[{log}] decode error: {e}")
        return None
    return envelope if isinstance(envelope, dict) else None


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
        # session id → envelope id of its in-flight session_start turn (C3).
        self._start_turns: dict[str, str] = {}

    async def on_message(self, msg: Any) -> None:
        envelope = _decode(msg, self.log)
        if envelope is None:
            return
        meta = envelope.get("meta") or {}
        cancel_id = cancel_request(envelope)
        if cancel_id is not None:
            if msg.subject == self.inbox_subject:  # cancels are DMs, not chatter
                await self._cancel(cancel_id, meta.get("from"))
            return
        self._enqueue(msg, envelope)

    async def on_session_message(self, msg: Any) -> None:
        """Callback for ``channel.<session channel>``: only session actions
        are work; the worker's own progress/results echo back and are dropped."""
        envelope = _decode(msg, self.log)
        if envelope is None:
            return
        payload = envelope.get("payload")
        if not isinstance(payload, dict) or payload.get("action") not in SESSION_CHANNEL_ACTIONS:
            return
        if (envelope.get("meta") or {}).get("kind") in NON_TASK_KINDS:
            return
        self._enqueue(msg, envelope)

    def _enqueue(self, msg: Any, envelope: dict[str, Any]) -> None:
        meta = envelope.get("meta") or {}
        payload = envelope.get("payload")
        payload = payload if isinstance(payload, dict) else {}
        task_id = meta.get("id")
        if meta.get("kind") not in NON_TASK_KINDS and task_id:
            owner = meta.get("from") or ""
            if payload.get("action") == "session_start" and self._dedupe_start(
                    payload.get("session_id"), task_id, owner):
                return
            self.registry.register(task_id, owner=owner)  # cancellable while queued
            self._queued[task_id] = envelope
            sid = payload.get("session_id")
            if payload.get("action") == "session_start" and isinstance(sid, str) and sid:
                self._start_turns[sid] = task_id
        self._queue.put_nowait((msg, envelope))

    def _dedupe_start(self, session_id: Any, task_id: str, owner: str) -> bool:
        """C3: a re-dispatched session_start while that session's turn is
        still queued or running aliases the running turn instead of a rerun."""
        if not isinstance(session_id, str) or not session_id:
            return False
        running = self._start_turns.get(session_id)
        if running is None or running not in self.registry:
            return False
        if not self.registry.alias(task_id, running, owner=owner):
            return False
        print(f"[{self.log}] session_start {task_id} for session {session_id} "
              f"while turn {running} is in flight: aliased, not re-run")
        return True

    async def _cancel(self, task_id: str, by: str | None) -> None:
        if task_id in self.registry and not self.registry.may_cancel(task_id, by):
            print(f"[{self.log}] cancel {task_id} from {by!r} ignored: "
                  f"not the task's sender ({self.registry.owner(task_id)!r})")
            return
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
                payload = envelope.get("payload")
                sid = payload.get("session_id") if isinstance(payload, dict) else None
                if isinstance(sid, str) and self._start_turns.get(sid) == task_id:
                    del self._start_turns[sid]
