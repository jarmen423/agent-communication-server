"""
ACP HTTP transport + WorkerBackend for remote Agent Client Protocol servers.

Implements the ACP Streamable HTTP + WebSocket transport RFD
(https://agentclientprotocol.com/rfds/streamable-http-websocket-transport).

``/acp`` endpoint contract:
  * POST — JSON-RPC. ``initialize`` returns 200 + JSON body (capabilities
    + connectionId). All others return 202 immediately; the response
    arrives on an SSE stream correlated by JSON-RPC ``id``.
  * GET (Accept: text/event-stream) — long-lived SSE stream.
    - Acp-Connection-Id only → connection-scoped stream.
    - Acp-Connection-Id + Acp-Session-Id → session-scoped stream.
  * GET (Upgrade: websocket) — full-duplex fallback (not implemented here;
    streamable HTTP is the primary path).  * DELETE — terminate.
HTTP/2 required by spec; falls back to HTTP/1.1 if the server does not
advertise h2. Cookies are persisted for sticky-session affinity.
"""

from __future__ import annotations

import asyncio
import json
import logging
import time
from typing import Any

import httpx

logger = logging.getLogger(__name__)

__all__ = ["AcpHttpTransport", "AcpHttpBackend", "AcpHttpError", "AcpHttpConnectionError"]


class AcpHttpError(RuntimeError):
    """ACP HTTP transport-level failure."""


class AcpHttpConnectionError(AcpHttpError):
    """Connection failure (DNS, TCP, TLS, refused)."""


class AcpHttpTransport:
    """Async JSON-RPC 2.0 client over ACP Streamable HTTP.

    Transport-only: knows JSON-RPC correlation and ACP headers, not ACP
    method semantics. Callers drive ``initialize``/``session/new``/
    ``session/prompt`` via :meth:`request` and receive streamed chunks
    via :meth:`set_notification_callback`.
    """

    PROTOCOL_VERSION = 1
    H_CONN = "Acp-Connection-Id"
    H_SESS = "Acp-Session-Id"

    def __init__(
        self,
        base_url: str,
        *,
        auth_headers: dict[str, str] | None = None,
        request_timeout_sec: float = 30.0,
        http2: bool = True,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.endpoint = f"{self.base_url}/acp"
        self.auth_headers = dict(auth_headers or {})
        self.request_timeout_sec = request_timeout_sec
        self.http2 = http2
        self._client: httpx.AsyncClient | None = None
        self._connection_id: str | None = None
        self._next_id = 1
        self._pending: dict[int, asyncio.Future[dict[str, Any]]] = {}
        self._lock = asyncio.Lock()
        self._conn_task: asyncio.Task[None] | None = None
        self._sess_tasks: dict[str, asyncio.Task[None]] = {}
        self._alive = False
        self._on_notification: Any = None

    async def start(self) -> None:
        """Open the HTTP client, run ``initialize``, and open the
        connection-scoped SSE stream."""
        async with self._lock:
            if self._client is not None:
                return
            try:
                self._client = httpx.AsyncClient(
                    http2=self.http2,
                    timeout=httpx.Timeout(self.request_timeout_sec),
                    headers={"Accept": "application/json"},
                    follow_redirects=True,
                )
            except Exception as e:
                raise AcpHttpConnectionError(f"client init: {e}") from e

            init_resp = await self._post("initialize", {}, session_id=None)
            payload = init_resp.get("result") or {}
            meta = init_resp.get("_meta") or {}
            conn_id = (init_resp.get(self.H_CONN)
                       or meta.get("connectionId")
                       or (payload.get("connectionId") if isinstance(payload, dict) else None))
            if not conn_id:
                raise AcpHttpError(f"initialize missing connectionId: {init_resp!r}")
            self._connection_id = str(conn_id)
            self._alive = True
            self._conn_task = asyncio.create_task(self._read_sse(None))

    async def close(self) -> None:
        async with self._lock:
            self._alive = False
            for t in list(self._sess_tasks.values()):
                t.cancel()
            self._sess_tasks.clear()
            if self._conn_task:
                self._conn_task.cancel()
                self._conn_task = None
            for fut in self._pending.values():
                if not fut.done():
                    fut.set_exception(AcpHttpError("transport closed"))
            self._pending.clear()
            if self._client:
                try:
                    await self._client.aclose()
                except Exception:
                    pass
                self._client = None

    def _headers(self, session_id: str | None) -> dict[str, str]:
        h = {"Content-Type": "application/json", "Accept": "application/json",
             **self.auth_headers}
        if self._connection_id:
            h[self.H_CONN] = self._connection_id
        if session_id:
            h[self.H_SESS] = session_id
        return h

    async def request(self, method: str, params: dict[str, Any] | None = None,
                      *, session_id: str | None = None,
                      timeout: float | None = None) -> dict[str, Any]:
        """Send a JSON-RPC request and await the correlated response.

        ``initialize`` returns its result inline in the 200 body; other
        methods return ``202 Accepted`` immediately and the real
        response arrives on the SSE stream correlated by JSON-RPC ``id``.
        Returns the JSON-RPC ``result`` payload (or empty dict on error).
        """
        resp = await self._post(method, params or {}, session_id=session_id,
                                timeout=timeout, wait_response=True)
        if isinstance(resp, dict):
            result = resp.get("result")
            return result if isinstance(result, dict) else {}
        return {}

    async def _post(self, method: str, params: dict[str, Any], *,
                    session_id: str | None, timeout: float | None = None,
                    wait_response: bool = True) -> dict[str, Any]:
        if not self._client:
            raise AcpHttpError("transport not started")
        req_id = self._next_id
        self._next_id += 1
        # ``initialize`` returns its result in the 200 body — never
        # goes through the pending-future machinery below.
        headers = ({"Content-Type": "application/json", "Accept": "application/json",
                    **self.auth_headers}
                   if method == "initialize" else self._headers(session_id))
        payload = {"jsonrpc": "2.0", "id": req_id, "method": method, "params": params}

        try:
            resp = await self._client.post(
                self.endpoint, content=json.dumps(payload), headers=headers,
                timeout=timeout or self.request_timeout_sec,
            )
        except httpx.RequestError as e:
            raise AcpHttpConnectionError(f"{method}: {e}") from e

        if method == "initialize":
            if resp.status_code != 200:
                raise AcpHttpError(f"initialize HTTP {resp.status_code}: {resp.text[:200]}")
            try:
                data = resp.json()
            except json.JSONDecodeError as e:
                raise AcpHttpError(f"initialize non-JSON: {resp.text[:200]}") from e
            if self.H_CONN in resp.headers:
                data.setdefault(self.H_CONN, resp.headers[self.H_CONN])
            return data
        if resp.status_code not in (200, 202):
            raise AcpHttpError(f"{method} HTTP {resp.status_code}: {resp.text[:200]}")
        if not wait_response:
            return {"_http_status": resp.status_code}
        loop = asyncio.get_running_loop()
        fut: asyncio.Future[dict[str, Any]] = loop.create_future()
        self._pending[req_id] = fut
        try:
            return await asyncio.wait_for(
                fut, timeout=timeout or self.request_timeout_sec
            )
        except asyncio.TimeoutError as e:
            self._pending.pop(req_id, None)
            raise AcpHttpError(f"{method} timed out waiting on SSE stream") from e

    async def open_session_stream(self, session_id: str) -> None:
        """Open the per-session SSE stream (idempotent)."""
        if not self._client or not self._connection_id:
            raise AcpHttpError("transport not started")
        if session_id not in self._sess_tasks:
            self._sess_tasks[session_id] = asyncio.create_task(
                self._read_sse(session_id)
            )

    async def close_session_stream(self, session_id: str) -> None:
        t = self._sess_tasks.pop(session_id, None)
        if t:
            t.cancel()
            try:
                await t
            except (asyncio.CancelledError, Exception):
                pass

    async def send_raw(self, payload: dict[str, Any], session_id: str | None = None,
                       timeout: float = 10.0) -> None:
        """Send a JSON-RPC payload without correlation (e.g. permission reply)."""
        if not self._client or not self._connection_id:
            return
        try:
            await self._client.post(
                self.endpoint, content=json.dumps(payload),
                headers=self._headers(session_id), timeout=timeout,
            )
        except Exception as e:
            logger.debug("[acp-http] raw POST failed: %s", e)

    def set_notification_callback(self, cb: Any) -> None:
        """``async def cb(method, params, raw)`` receives every inbound
        notification and server-initiated request from the SSE streams."""
        self._on_notification = cb

    async def _read_sse(self, session_id: str | None) -> None:
        assert self._client and self._connection_id
        headers = {"Accept": "text/event-stream", **self.auth_headers,
                   self.H_CONN: self._connection_id}
        if session_id is not None:
            headers[self.H_SESS] = session_id
        backoff = 1.0
        try:
            while self._alive:
                try:
                    async with self._client.stream(
                        "GET", self.endpoint, headers=headers
                    ) as resp:
                        if resp.status_code not in (200, 206):
                            logger.warning("[acp-http] SSE %s, retrying", resp.status_code)
                            await asyncio.sleep(backoff)
                            backoff = min(backoff * 2, 15.0)
                            continue
                        backoff = 1.0
                        buf: list[str] = []
                        async for line in resp.aiter_lines():
                            if line == "" and buf:
                                raw = "\n".join(buf).strip()
                                buf = []
                                try:
                                    await self._route(json.loads(raw))
                                except json.JSONDecodeError:
                                    logger.debug("[acp-http] non-json SSE: %s", raw[:200])
                            elif line.startswith("data:"):
                                buf.append(line[5:].lstrip())
                except asyncio.CancelledError:
                    return
                except Exception as e:
                    logger.warning("[acp-http] SSE %s: %s; retrying",
                                   type(e).__name__, e)
                    await asyncio.sleep(backoff)
                    backoff = min(backoff * 2, 15.0)
        except asyncio.CancelledError:
            return

    async def _route(self, msg: dict[str, Any]) -> None:
        if "id" in msg and ("result" in msg or "error" in msg):
            fut = self._pending.pop(msg["id"], None)
            if fut and not fut.done():
                if "error" in msg:
                    err = msg["error"]
                    fut.set_exception(AcpHttpError(
                        err.get("message") if isinstance(err, dict) else str(err)
                    ))
                else:
                    fut.set_result(msg)
            return
        method = msg.get("method")
        if not method:
            return
        if self._on_notification is None:
            return
        try:
            await self._on_notification(method, msg.get("params") or {}, msg)
        except Exception as e:
            logger.debug("[acp-http] notif callback raised: %s", e)


# ── Backend


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
        self._user_handler: Any = None
        self._lock = asyncio.Lock()

    def set_progress_handler(self, handler: Any) -> None:
        """``async def handler(kind, data)`` matching the ``grok_acp.py``
        shape (``kind`` in ``message``/``thought``/``tool``). Chunks are
        throttled to avoid flooding NATS."""
        self._user_handler = handler

    def clear_progress_handler(self) -> None:
        self._user_handler = None
        self._stream_buf = ""
        self._thought_buf = ""

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
        if self._user_handler is None:
            return
        try:
            await self._user_handler(method, {"method": method, "params": params})
        except Exception:
            pass

    async def _emit(self, kind: str, text: str, *, force: bool = False) -> None:
        """Throttled stream forwarder — mirrors ``grok_acp.py``."""
        if self._user_handler is None:
            return
        now = time.monotonic()
        if kind == "tool":
            try:
                await self._user_handler("tool", {"text": text, "message": text})
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
                await self._user_handler(
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
        async with self._lock:
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

            result = await self.transport.request(
                "session/prompt",
                {"sessionId": session_id,
                 "prompt": [{"type": "text", "text": prompt}]},
                session_id=session_id,
                timeout=self.request_timeout_sec,
            )
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
