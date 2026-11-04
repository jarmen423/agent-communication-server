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
  not run twice (C3): its envelope id becomes an alias of that turn;
- delivery is at-least-once on a ``--durable`` hub: an envelope whose
  ``meta.id`` was already seen (a redelivery) is dropped here.
"""

from __future__ import annotations

import asyncio
import json
import logging
import re
from typing import Any, Awaitable, Callable

from hub_delivery import SeenIds
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
        self.accept_check = None
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
        self._seen = SeenIds()  # redelivered meta.ids (at-least-once delivery)

    async def on_message(self, msg: Any) -> None:
        envelope = _decode(msg, self.log)
        if envelope is None:
            return
        meta = envelope.get("meta") or {}
        if not self._seen.first_sight(meta.get("id")):
            logger.debug("duplicate envelope %s dropped", meta.get("id"))
            return
        if (self.accept_check is not None
                and not await self.accept_check.allows(envelope)):
            return
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
        if ((envelope.get("meta") or {}).get("kind") in NON_TASK_KINDS
                and not (payload.get("action") == "session_close"
                         and (envelope.get("meta") or {}).get("kind") == "control")):
            return
        if self.accept_check is not None and not await self.accept_check.allows(envelope):
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
        still queued or running aliases the running turn instead of a rerun.
        One from anyone but the turn's sender is dropped (C5). True = handled."""
        if not isinstance(session_id, str) or not session_id:
            return False
        running = self._start_turns.get(session_id)
        if running is None or running not in self.registry:
            return False
        if owner != self.registry.owner(running):  # C5: not the session's owner
            print(f"[{self.log}] session_start {task_id} for session {session_id} from "
                  f"{owner!r} ignored: owner is {self.registry.owner(running)!r}")
            return True
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
                if (self.accept_check is not None
                        and not await self.accept_check.allows(envelope)):
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


def task_channel(channel: Any) -> bool:
    return isinstance(channel, str) and bool(re.fullmatch(
        r"(?:task\.[A-Za-z0-9_-]+|wave\.[A-Za-z0-9_-]+\.task\.[A-Za-z0-9_-]+)", channel))


def policy_allows(record: dict, origin: Any) -> bool:
    if not isinstance(origin, dict):
        return False
    user = ((origin.get("on_behalf_of") or {}).get("user")
            if origin.get("identity") == "hub-orchestrator" else origin.get("user"))
    policy = record.get("accept")
    if not policy:
        human = not record.get("agent") and record.get("user") == record.get("identity")
        policy = {"from": "tenant" if human or record.get("identity") == "owner"
                  or record.get("owner_kind") == "tenant" else "owner"}
    if policy.get("from") == "tenant":
        return (origin.get("owner_kind") is not None or
                (origin.get("identity") == "hub-orchestrator" and
                 bool((origin.get("on_behalf_of") or {}).get("identity"))))
    if policy.get("from") == "users":
        return user is not None and user in policy.get("users", [])
    return user is not None and user == record.get("user")


class AcceptCheck:
    """Cache the last good self record; API errors never erase it."""
    def __init__(self, request, publish):
        self.request = request
        self.publish = publish
        self.record = None

    async def refresh(self):
        try:
            reply = await self.request("manifest.self", {})
            if reply.get("ok"):
                data = reply.get("data")
                self.record = (data.get("record") if isinstance(data, dict)
                               and data.get("enforced") is True else None)
        except Exception:
            logger.debug("manifest.self unavailable", exc_info=True)

    async def allows(self, envelope):
        payload = envelope.get("payload")
        payload = payload if isinstance(payload, dict) else {}
        action = payload.get("action")
        if (cancel_request(envelope) is not None or
                (action == "session_close" and
                 (envelope.get("meta") or {}).get("kind") == "control") or
                (action not in ("session_start", "session_send") and
                 (envelope.get("meta") or {}).get("kind") in NON_TASK_KINDS)):
            return True
        if self.record is None or policy_allows(self.record, envelope.get("meta", {}).get("origin")):
            return True
        meta = envelope.get("meta") or {}
        channel = payload.get("task_channel")
        if not isinstance(channel, str):
            channel = meta.get("channel")
        session_refusal = (action == "session_start" and
                           isinstance(payload.get("session_channel"), str) and
                           re.fullmatch(r"session\.[A-Za-z0-9_-]+", payload["session_channel"]))
        if session_refusal:
            channel = payload["session_channel"]
        if task_channel(channel) or session_refusal:
            task_id = next((payload[k] for k in ("task_id", "session_id")
                            if isinstance(payload.get(k), str)
                            and 0 < len(payload[k]) <= 128
                            and all("!" <= c <= "~" for c in payload[k])), meta.get("id"))
            origin = meta.get("origin")
            origin = origin if isinstance(origin, dict) else {}
            sender = ((origin.get("on_behalf_of") or {}).get("identity")
                      if origin.get("identity") == "hub-orchestrator" else origin.get("identity"))
            await self.publish(channel, "status" if session_refusal else "message", {
                "refused_sender": sender, "message_id": meta.get("id"),
                "status": "error", "task_id": task_id, "result": None,
                "error": "refused by worker accept policy", "rejected": True,
            }, reply_to=meta.get("id"))
        return False
