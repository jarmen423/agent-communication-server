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
    """task id → cancel handle, for queued and running turns."""

    def __init__(self) -> None:
        self._handles: dict[str, TurnCancel] = {}

    def register(self, task_id: str | None) -> TurnCancel:
        handle = TurnCancel()
        if task_id:
            self._handles[task_id] = handle
        return handle

    def get(self, task_id: str | None) -> TurnCancel | None:
        return self._handles.get(task_id) if task_id else None

    def finish(self, task_id: str | None) -> None:
        if task_id:
            self._handles.pop(task_id, None)

    def cancel(self, task_id: str, by: str | None = None) -> bool:
        """Cancel ``task_id``. False when it is unknown or already finished."""
        handle = self._handles.get(task_id)
        if handle is None:
            return False
        handle.cancel(by)
        return True

    def __contains__(self, task_id: object) -> bool:
        return task_id in self._handles

    def __len__(self) -> int:
        return len(self._handles)
