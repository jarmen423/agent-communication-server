"""High-level ACP client over :class:`AcpHttpTransport` (``acp_http.py``).

Implements the nats-hub ``WorkerBackend`` protocol. Progress goes to the
handler of the turn holding the lock (``worker_backends.progress``), and a
cancelled turn sends the ACP ``session/cancel`` notification
(refocus-iteration-2.md §4.2). The agent runs remotely, so there is no local
process group to kill.
"""

from __future__ import annotations

import asyncio
import json
import logging
import time
from typing import Any

from worker_backends.acp_http import AcpHttpError, AcpHttpTransport
from worker_backends.progress import current_progress_handler

logger = logging.getLogger(__name__)

__all__ = ["AcpHttpBackend"]


class AcpHttpBackend:
    """High-level ACP client backed by :class:`AcpHttpTransport`.

    Implements the nats-hub ``WorkerBackend`` protocol::

        async def run(self, prompt: str, ctx: dict) -> tuple[str, dict]:

    Lifecycle: ``initialize`` (once) → ``session/new`` (if no
    ``acp_session_id`` in ctx) → open session-scoped GET stream →
    ``session/prompt``. Responses + streamed ``session/update``
    notifications arrive on the SSE stream correlated by ``id``.
    Streamed ``agent_message_chunk`` text is concatenated → returned.
    """

    def __init__(
        self,
        base_url: str,
        *,
        cwd: str | None = None,
        model: str | None = None,
        auth_headers: dict[str, str] | None = None,
        log_label: str = "acp-http",
        request_timeout_sec: float = 900.0,
        always_approve: bool = True,
        http2: bool = True,
    ) -> None:
        self.base_url = base_url
        self.cwd = cwd
        self.model = model
        self.log_label = log_label
        self.request_timeout_sec = request_timeout_sec
        self.always_approve = always_approve
        self.transport = AcpHttpTransport(
            base_url, auth_headers=auth_headers,
            request_timeout_sec=request_timeout_sec, http2=http2,
        )
        self.transport.set_notification_callback(self._on_inbound)
        self._started = False
        self._chunks: list[str] = []
        # Throttling knobs (mirror grok_acp.py).
        self._stream_buf = ""
        self._thought_buf = ""
        self._last_stream_emit: float = 0.0
        self._stream_min_interval = 0.35
        self._stream_min_chars = 40
        self._user_handler: Any = None  # fallback for direct callers
        self._turn_handler: Any = None  # handler of the turn holding _lock
        self._lock = asyncio.Lock()

    def set_progress_handler(self, handler: Any) -> None:
        """Fallback ``async def handler(kind, data)`` for direct callers; the
        runtime passes a per-turn handler via ``worker_backends.progress``.
        Chunks are throttled to avoid flooding NATS."""
        self._user_handler = handler

    def clear_progress_handler(self) -> None:
        self._user_handler = None

    async def _on_inbound(self, method: str, params: dict[str, Any],
                          raw: dict[str, Any]) -> None:
        # Server-initiated request (e.g. request_permission): auto-ack.
        if "id" in raw and method in {"request_permission",
                                       "session/request_permission"}:
            if self.always_approve:
                await self.transport.send_raw({
                    "jsonrpc": "2.0", "id": raw["id"],
                    "result": {"outcome": {"outcome": "selected",
                                           "optionId": "allow-always"}},
                })
            return
        if method in {"session/update", "notifications/session/update"}:
            update = (params or {}).get("update") or {}
            kind = update.get("sessionUpdate") or update.get("type")
            if kind in {"agent_message_chunk", "agent_message",
                         "agent_thought_chunk", "agent_thought"}:
                content = update.get("content") or {}
                text = content.get("text") if isinstance(content, dict) else None
                if not text:
                    return
                if kind.startswith("agent_message"):
                    self._chunks.append(str(text))
                    await self._emit("message", str(text))
                else:
                    await self._emit("thought", str(text))
                return
            if kind in {"tool_call", "tool_call_update", "agent_tool_call",
                        "tool_call_start"}:
                title = (update.get("title") or update.get("toolName")
                         or update.get("name") or str(kind))
                status = update.get("status") or update.get("kind") or ""
                await self._emit("tool", f"tool: {title} {status}".strip(), force=True)
                return
        if self._turn_handler is None:
            return
        try:
            await self._turn_handler(method, {"method": method, "params": params})
        except Exception:
            pass

    async def _emit(self, kind: str, text: str, *, force: bool = False) -> None:
        """Throttled stream forwarder — mirrors ``grok_acp.py``."""
        handler = self._turn_handler
        if handler is None:
            return
        now = time.monotonic()
        if kind == "tool":
            try:
                await handler("tool", {"text": text, "message": text})
            except Exception as e:
                logger.debug("[%s] progress handler error: %s", self.log_label, e)
            self._last_stream_emit = now
            return
        buf_attr = "_thought_buf" if kind == "thought" else "_stream_buf"
        buf = getattr(self, buf_attr)
        if text:
            setattr(self, buf_attr, buf + text)
            buf = getattr(self, buf_attr)
        if not buf:
            return
        due = (now - self._last_stream_emit) >= self._stream_min_interval
        if force or due or len(buf) >= self._stream_min_chars:
            try:
                await handler(
                    kind if kind in ("message", "thought") else "message",
                    {"text": buf[-800:], "delta": text, "full_len": len(buf)},
                )
            except Exception as e:
                logger.debug("[%s] progress handler error: %s", self.log_label, e)
            self._last_stream_emit = now

    async def start(self) -> None:
        if self._started:
            return
        await self.transport.start()
        self._started = True

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        handler = current_progress_handler(self._user_handler)
        async with self._lock:
            try:
                self._turn_handler = handler
                return await self._run_locked(prompt, ctx)
            finally:
                self._turn_handler = None
                self._stream_buf = self._thought_buf = ""

    async def _run_locked(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        if not self._started:
            await self.start()

        session_id = ctx.get("acp_session_id")
        if not session_id:
            params: dict[str, Any] = {"cwd": self.cwd or "", "mcpServers": []}
            if self.model:
                params["model"] = self.model
            new_resp = await self.transport.request(
                "session/new", params, session_id=None, timeout=30.0
            )
            session_id = new_resp.get("sessionId") or new_resp.get("session_id")
            if not session_id:
                raise AcpHttpError(f"session/new no sessionId: {new_resp!r}")
            ctx = dict(ctx or {})
            ctx["acp_session_id"] = session_id
            logger.info("[%s] session %s", self.log_label, str(session_id)[:12])
            await self.transport.open_session_stream(session_id)

        self._chunks.clear()
        self._stream_buf = ""
        self._thought_buf = ""
        self._last_stream_emit = 0.0
        logger.info("[%s] prompt: %s", self.log_label, prompt[:100])

        try:
            result = await self.transport.request(
                "session/prompt",
                {"sessionId": session_id,
                 "prompt": [{"type": "text", "text": prompt}]},
                session_id=session_id,
                timeout=self.request_timeout_sec,
            )
        except asyncio.CancelledError:
            # Hub cancel (§4.2): ACP session/cancel is a notification (no id).
            await asyncio.shield(self.transport.send_raw(
                {"jsonrpc": "2.0", "method": "session/cancel",
                 "params": {"sessionId": session_id}}, session_id=session_id))
            raise
        if self._stream_buf:
            await self._emit("message", "", force=True)
        if self._thought_buf:
            await self._emit("thought", "", force=True)

        text = "".join(self._chunks).strip()
        if not text and isinstance(result, dict):
            fb = result.get("text") or result.get("result") or result.get("message") or ""
            if isinstance(fb, dict):
                fb = fb.get("text") or json.dumps(fb)
            text = str(fb).strip()
        if not text:
            stop = result.get("stopReason") if isinstance(result, dict) else None
            text = f"(no text returned; stopReason={stop})"
        return text, ctx

    async def close(self) -> None:
        self._started = False
        await self.transport.close()
