"""Running-task bookkeeping for the cancel contract (refocus-iteration-2.md §4.2).

1. The canceller DMs the worker ``kind = control``,
   payload ``{"action": "cancel", "task_id": <task envelope id>}``.
2. A running task is stopped (its backend turn is cancelled, which kills the
   CLI's process group or sends ACP ``session/cancel``) and the worker
   publishes one terminal result with ``status: "cancelled"``.
3. Unknown or finished tasks are ignored (debug log, no reply).

A task that is queued but not started yet is also cancellable: it is marked
here and reports ``cancelled`` as soon as the dispatcher reaches it, without
running the backend.

Sender check (HARDENING.md C5): a cancel is honoured only when its
``meta.from`` equals ``meta.from`` of the task envelope it names (the
*owner* recorded at registration). In permissive mode ``meta.from`` is
self-asserted, so this is best effort; on a ``--require-bound-identity`` hub
the router stamps it and the check is real.

Aliases (C3): a re-dispatched ``session_start`` for a session whose turn is
still in flight is registered as an alias of that turn's handle, so a cancel
by either envelope id stops the one running turn.
"""

from __future__ import annotations

import asyncio
from typing import Any


class TurnCancel:
    """Cancel handle for one backend turn.

    ``execute_with_events`` attaches the asyncio task that runs
    ``backend.run``; :meth:`cancel` cancels exactly that task (never the
    caller), and ``requested`` tells the caller a CancelledError was ours
    rather than a worker shutdown.
    """

    def __init__(self) -> None:
        self.requested = False
        self.by: str | None = None
        self._task: asyncio.Future[Any] | None = None

    def attach(self, task: asyncio.Future[Any]) -> None:
        self._task = task
        if self.requested:
            task.cancel()

    def cancel(self, by: str | None = None) -> None:
        self.requested = True
        self.by = self.by or by
        if self._task is not None and not self._task.done():
            self._task.cancel()


def cancel_request(envelope: dict[str, Any]) -> str | None:
    """The task id if ``envelope`` is a §4.2 cancel request, else None."""
    meta = envelope.get("meta") or {}
    payload = envelope.get("payload")
    if meta.get("kind") != "control" or not isinstance(payload, dict):
        return None
    if payload.get("action") != "cancel":
        return None
    task_id = payload.get("task_id")
    return task_id if isinstance(task_id, str) and task_id else None


class TaskRegistry:
    """task id → cancel handle (+ owner), for queued and running turns."""

    def __init__(self) -> None:
        self._handles: dict[str, TurnCancel] = {}
        self._owners: dict[str, str | None] = {}
        self._aliases: dict[str, list[str]] = {}  # primary id → alias ids

    def register(self, task_id: str | None, owner: str | None = None) -> TurnCancel:
        handle = TurnCancel()
        if task_id:
            self._handles[task_id] = handle
            self._owners[task_id] = owner
        return handle

    def alias(self, alias_id: str, task_id: str, owner: str | None = None) -> bool:
        """Make ``alias_id`` cancel the same turn as ``task_id`` (C3). The
        alias keeps its own owner (the sender of the re-dispatch)."""
        handle = self._handles.get(task_id)
        if handle is None or not alias_id or alias_id == task_id:
            return False
        self._handles[alias_id] = handle
        self._owners[alias_id] = owner
        self._aliases.setdefault(task_id, []).append(alias_id)
        return True

    def get(self, task_id: str | None) -> TurnCancel | None:
        return self._handles.get(task_id) if task_id else None

    def owner(self, task_id: str | None) -> str | None:
        return self._owners.get(task_id) if task_id else None

    def finish(self, task_id: str | None) -> None:
        if not task_id:
            return
        for tid in (task_id, *self._aliases.pop(task_id, ())):
            self._handles.pop(tid, None)
            self._owners.pop(tid, None)

    def cancel(self, task_id: str, by: str | None = None) -> bool:
        """Cancel ``task_id``. False when it is unknown, already finished, or
        ``by`` is not the sender of the task envelope (C5)."""
        handle = self._handles.get(task_id)
        if handle is None or not self.may_cancel(task_id, by):
            return False
        handle.cancel(by)
        return True

    def may_cancel(self, task_id: str, by: str | None) -> bool:
        """C5: only the task's own sender may cancel it."""
        owner = self._owners.get(task_id)
        return owner is None or (by is not None and by == owner)

    def __contains__(self, task_id: object) -> bool:
        return task_id in self._handles

    def __len__(self) -> int:
        return len(self._handles)
