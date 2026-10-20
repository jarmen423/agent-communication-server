"""
AcpAgent backend — wired when an ACP server exists in this environment.

Current status:
- Hermes: real stdio JSON-RPC verified in worker_backends/hermes_acp.py
- Grok: real stdio JSON-RPC in worker_backends/grok_acp.py (`grok agent stdio`)
- Cursor: not yet an available stdio/HTTP endpoint here; blocked until
  a Cursor ACP process/transport is exposed in this environment.

This module avoids placeholder success paths. If no transport is
reachable, it fails fast with a clear error instead of pretending to work.

Cancel (refocus-iteration-2.md §4.2): a cancelled turn calls the transport's
optional ``cancel_turn(session_handle)`` (e.g. ACP ``session/cancel``) before
the cancellation propagates.
"""

from __future__ import annotations

import asyncio
import logging
from typing import Any

from worker_runtime import WorkerBackend

logger = logging.getLogger(__name__)


class AcpAgentTransport:
    async def send_turn(
        self, prompt: str, session_handle: str | None
    ) -> tuple[str, str]:
        """Return (assistant_text, new_or_existing_session_handle)."""
        raise NotImplementedError

    async def cancel_turn(self, session_handle: str | None) -> None:
        """Stop the in-flight turn (optional; default: nothing to stop)."""


class AcpAgentBackend(WorkerBackend):
    """
    ACP backend for any agent client that exposes a stdio/HTTP/WebSocket
    JSON-RPC 2.0 transport.

    Example for Hermes today:
        from worker_backends.hermes_acp import HermesAcpBackend
        backend = HermesAcpBackend(model="claude-sonnet-4")
    """

    def __init__(self, transport: AcpAgentTransport, label: str = "acp-agent") -> None:
        self.transport = transport
        self.label = label

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        session_handle = ctx.get("acp_session_handle")
        try:
            text, session_handle = await self.transport.send_turn(prompt, session_handle)
        except asyncio.CancelledError:
            await asyncio.shield(self.transport.cancel_turn(session_handle))
            raise
        except NotImplementedError as e:
            raise RuntimeError(
                "No ACP transport wired. Use a concrete backend like "
                "`worker_backends.hermes_acp.HermesAcpBackend`. "
                f"Label={self.label} detail={e}"
            ) from e

        ctx = dict(ctx or {})
        ctx["acp_session_handle"] = session_handle
        return text, ctx