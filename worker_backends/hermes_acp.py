"""
Hermes ACP backend — speaks JSON-RPC 2.0 over stdio to `hermes acp`.

Much cleaner than parsing `hermes chat -q` stdout: proper session
management, streaming, tool progress, and the full ACP protocol.
"""

from __future__ import annotations

import asyncio
import logging
import os
from typing import Any

from acp import (
    NewSessionRequest,
    PromptRequest,
    LoadSessionRequest,
    spawn_stdio_connection,
    text_block,
)
from acp.schema import (
    AgentMessageChunk,
    AgentThoughtChunk,
    TextContentBlock,
)

logger = logging.getLogger(__name__)


class HermesAcpBackend:
    """
    ACP backend for Hermes Agent.

    Spawns `hermes acp` as a long-lived subprocess and communicates via
    JSON-RPC 2.0 over stdio. Sessions survive across turns (no stdout
    parsing, no --resume hacks).

    Usage::

        backend = HermesAcpBackend(model="claude-sonnet-4")
        await backend.start()
        text, ctx = await backend.run("What is 2+2?", {})
        text, ctx = await backend.run("And 3+3?", ctx)
        await backend.close()
    """

    def __init__(
        self,
        model: str | None = None,
        cwd: str | None = None,
        hermes_cmd: str = "hermes",
        acp_args: list[str] | None = None,
        log_label: str = "hermes-acp",
    ) -> None:
        self.model = model
        self.cwd = cwd or os.getcwd()
        self.hermes_cmd = hermes_cmd
        self.acp_args = acp_args or []
        self.log_label = log_label

        self._connection = None
        self._process = None
        self._started = False
        self._message_id = 0

    async def start(self) -> None:
        """Spawn `hermes acp` and establish the ACP connection."""
        if self._started:
            return

        cmd = [self.hermes_cmd, "acp", *self.acp_args]

        # Collect streamed text from agent responses
        collected_chunks: list[str] = []

        async def _handler(method: str, params: Any, is_notification: bool) -> Any:
            """Handle server→client requests (tool approvals, etc.)."""
            # For now, auto-approve everything (headless mode)
            if method == "request_permission":
                return {"approved": True}
            return None

        async def _observer(event) -> None:
            """Collect streamed agent message chunks via session/update notifications."""
            msg = event.message
            if not isinstance(msg, dict):
                return
            method = msg.get("method", "")
            params = msg.get("params", {})

            # New protocol: session/update notifications carry typed updates
            if method == "session/update":
                update = params.get("update", {})
                update_type = update.get("sessionUpdate", "")

                if update_type == "agent_message_chunk":
                    content = update.get("content", {})
                    if isinstance(content, dict) and content.get("type") == "text":
                        collected_chunks.append(content.get("text", ""))

                elif update_type == "agent_thought_chunk":
                    content = update.get("content", {})
                    if isinstance(content, dict) and content.get("type") == "text":
                        logger.debug("[thinking] %s", content.get("text", "")[:200])

        # Spawn the ACP server
        logger.info("[%s] spawning %s", self.log_label, " ".join(cmd))

        self._conn_ctx = spawn_stdio_connection(
            _handler,
            cmd[0],
            *cmd[1:],
            cwd=self.cwd,
            observers=[_observer],
        )
        self._connection, self._process = await self._conn_ctx.__aenter__()
        self._started = True
        self._collected = collected_chunks

        # Initialize the ACP session
        logger.info("[%s] ACP connection established", self.log_label)

    def _next_id(self) -> str:
        self._message_id += 1
        return f"msg-{self._message_id}"

    async def _send_request(self, method: str, params: dict[str, Any] | None = None) -> Any:
        """Send a JSON-RPC request and wait for the response."""
        if not self._started:
            await self.start()
        return await self._connection.send_request(method, params)

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        """
        Send a prompt to Hermes via ACP and return the response.

        ctx keys:
          - acp_session_id: ACP session ID (set after first turn)
          - acp_model: model override (optional)
        """
        if not self._started:
            await self.start()

        session_id = ctx.get("acp_session_id")

        if session_id:
            # Resume existing session
            logger.info("[%s] resuming session %s", self.log_label, session_id[:12])
            try:
                await self._send_request(
                    "session/load",
                    LoadSessionRequest(sessionId=session_id, cwd=self.cwd, mcpServers=[]).model_dump(by_alias=True),
                )
            except Exception as e:
                # Session may have expired or been cleaned up — create new
                logger.warning("[%s] load failed (%s), creating new session", self.log_label, e)
                session_id = None

        if not session_id:
            # Create new session
            logger.info("[%s] creating new ACP session", self.log_label)
            new_req = NewSessionRequest(cwd=self.cwd, mcpServers=[])
            resp = await self._send_request("session/new", new_req.model_dump(by_alias=True))
            session_id = resp.get("sessionId") if isinstance(resp, dict) else getattr(resp, "session_id", None)
            if not session_id:
                raise RuntimeError(f"No sessionId in new_session response: {resp}")
            ctx["acp_session_id"] = session_id
            logger.info("[%s] session created: %s", self.log_label, session_id[:12])

        # Set model if specified
        if self.model and self.model != ctx.get("acp_model"):
            try:
                await self._send_request(
                    "session/set_model",
                    {"sessionId": session_id, "modelId": self.model},
                )
                ctx["acp_model"] = self.model
            except Exception:
                pass  # Model set may not be supported on all ACP versions

        # Clear collected chunks for this turn
        self._collected.clear()

        # Send the prompt
        msg_id = self._next_id()
        prompt_req = PromptRequest(
            messageId=msg_id,
            sessionId=session_id,
            prompt=[text_block(prompt)],
        )

        logger.info("[%s] prompt: %s", self.log_label, prompt[:80])
        resp = await self._send_request("session/prompt", prompt_req.model_dump(by_alias=True))

        # Build the response text from collected chunks
        if self._collected:
            text = "".join(self._collected)
        elif isinstance(resp, dict):
            # Fallback: extract text from response
            text = resp.get("text") or resp.get("result") or str(resp)
        else:
            text = str(resp) if resp else "(empty response)"

        return text.strip(), ctx

    async def close(self) -> None:
        """Shut down the ACP connection and subprocess."""
        if self._conn_ctx and self._connection:
            try:
                await self._conn_ctx.__aexit__(None, None, None)
            except Exception:
                pass
        self._started = False
        self._connection = None
        self._process = None