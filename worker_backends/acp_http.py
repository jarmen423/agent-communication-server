"""
ACP HTTP transport for remote Agent Client Protocol servers (the WorkerBackend
built on it is `acp_http_backend.AcpHttpBackend`).

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
from typing import Any

import httpx

logger = logging.getLogger(__name__)

__all__ = ["AcpHttpTransport", "AcpHttpError", "AcpHttpConnectionError"]


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


def __getattr__(name: str):  # backward compat: AcpHttpBackend moved out (LOC split)
    if name == "AcpHttpBackend":
        from worker_backends.acp_http_backend import AcpHttpBackend

        return AcpHttpBackend
    raise AttributeError(name)
