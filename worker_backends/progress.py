"""Per-turn progress handlers (no cross-talk between concurrent turns).

``worker_events.execute_with_events`` runs every backend turn in its own
asyncio task with :data:`PROGRESS_HANDLER` set to that turn's handler.
Backends read it with :func:`current_progress_handler` at the start of
``run()``, so two turns running concurrently on one shared backend object
(two hub sessions on the same worker) each stream to their own channel.

The older ``backend.set_progress_handler(h)`` API is still honoured as a
fallback (tests, direct callers), but the runtime no longer uses it: a
handler stored on the shared backend is exactly what caused the cross-talk.
"""

from __future__ import annotations

import contextvars
from typing import Any, Awaitable, Callable

ProgressHandler = Callable[[str, dict[str, Any]], Awaitable[None]]

PROGRESS_HANDLER: contextvars.ContextVar[ProgressHandler | None] = contextvars.ContextVar(
    "nats_hub_progress_handler", default=None
)


def current_progress_handler(fallback: ProgressHandler | None = None) -> ProgressHandler | None:
    """The handler for the turn running in this task (else ``fallback``)."""
    return PROGRESS_HANDLER.get() or fallback
