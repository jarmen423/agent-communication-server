"""In-process SDK agents (Cursor, future ACP clients). Blocking run in thread pool."""

from __future__ import annotations

import asyncio
from typing import Any, Callable


class SdkAgentBackend:
    """
    Wraps a sync (prompt, ctx) -> (text, ctx) callable for async worker_runtime.

    Use for Cursor SDK, Cline Python, or any ACP client that is not a shell one-liner.
    """

    def __init__(
        self,
        run_sync: Callable[[str, dict[str, Any]], tuple[str, dict[str, Any]]],
        log_label: str = "sdk-agent",
    ) -> None:
        self._run_sync = run_sync
        self.log_label = log_label

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        loop = asyncio.get_event_loop()
        return await loop.run_in_executor(None, self._run_sync, prompt, dict(ctx))