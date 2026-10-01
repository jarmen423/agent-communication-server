"""In-process SDK agents (Cursor, future ACP clients). Blocking run in thread pool."""

from __future__ import annotations

import asyncio
import logging
from typing import Any, Callable

logger = logging.getLogger(__name__)


class SdkAgentBackend:
    """
    Wraps a sync (prompt, ctx) -> (text, ctx) callable for async worker_runtime.

    Use for Cursor SDK, Cline Python, or any ACP client that is not a shell one-liner.

    Cancel (refocus-iteration-2.md §4.2): a Python thread cannot be killed, so
    a cancelled turn is abandoned. The hub gets its ``cancelled`` result at
    once and the thread's eventual result is discarded. Pass ``cancel_sync``
    when the SDK has its own stop call; it runs (in the pool) on cancel.
    """

    def __init__(
        self,
        run_sync: Callable[[str, dict[str, Any]], tuple[str, dict[str, Any]]],
        log_label: str = "sdk-agent",
        cancel_sync: Callable[[], None] | None = None,
    ) -> None:
        self._run_sync = run_sync
        self._cancel_sync = cancel_sync
        self.log_label = log_label

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        loop = asyncio.get_running_loop()
        try:
            return await loop.run_in_executor(None, self._run_sync, prompt, dict(ctx))
        except asyncio.CancelledError:
            if self._cancel_sync is not None:
                try:
                    await asyncio.shield(loop.run_in_executor(None, self._cancel_sync))
                except Exception as e:  # noqa: BLE001 - best effort
                    logger.warning("[%s] cancel_sync failed: %s", self.log_label, e)
            else:
                logger.info("[%s] turn cancelled; SDK thread abandoned", self.log_label)
            raise
