"""
ACP / protocol agents (placeholder).

When an Agent Client Protocol transport lands (stdio, HTTP, WebSocket), implement
AcpAgentBackend here: same WorkerBackend.run(prompt, ctx) contract, different wire.

Today: Cline uses worker.js (Node + @cline/sdk); not yet on worker_runtime.
"""

from __future__ import annotations

from typing import Any, Protocol


class AcpTransport(Protocol):
    async def send_turn(
        self, prompt: str, session_handle: str | None
    ) -> tuple[str, str]: ...


class AcpAgentBackend:
    """Stub — wire AcpTransport when ACP endpoint is defined."""

    def __init__(self, transport: AcpTransport, log_label: str = "acp-agent") -> None:
        self.transport = transport
        self.log_label = log_label

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        handle = ctx.get("acp_session")
        text, new_handle = await self.transport.send_turn(prompt, handle)
        ctx["acp_session"] = new_handle
        return text, ctx