"""
Kilo ACP HTTP backend — JSON-RPC 2.0 over streamable HTTP to `kilo acp --port`.

Kilo ships an ACP server (`kilo acp --port <port> --hostname <host> --cwd <dir>`)
that speaks the standard ACP protocol over HTTP POST/SSE — the same wire shape
that :mod:`acp_http` already implements. This backend is a thin specialization:

* builds the Kilo-specific ``/acp`` base URL via :func:`resolve_kilo_url`,
* surfaces an optional Bearer token from ``KILO_API_KEY`` / ``kilo auth login``
  (gracefully handled — auth is a no-op when the server doesn't demand it),
* persists the session id under the Kilo-namespaced ctx key
  ``kilo_acp_session_id`` so a multi-turn ``hub-session`` resumes cleanly,
* proxies progress events through :class:`AcpHttpBackend`'s throttled handler,
* falls back to ``session/set_model`` when ``session/new`` declined to bind
  a model (some Kilo versions accept the model only via that follow-up call).

All JSON-RPC plumbing, SSE correlation, and notification routing live in
``acp_http.py``. Do not re-implement it here.
"""

from __future__ import annotations

import logging
import os
from typing import Any

from worker_backends.acp_http import (
    AcpHttpBackend,
    AcpHttpError,
)

logger = logging.getLogger(__name__)

__all__ = ["KiloAcpBackend", "resolve_kilo_url"]


DEFAULT_KILO_PORT = 8721
SESSION_CTX_KEY = "kilo_acp_session_id"
SESSION_CTX_KEY_LEGACY = "acp_session_id"  # parity with hermes/grok backends


def resolve_kilo_url(port: int | str = DEFAULT_KILO_PORT,
                     hostname: str = "127.0.0.1",
                     *, scheme: str = "http",
                     path: str = "/acp") -> str:
    """Build the Kilo ACP HTTP server URL.

    Default is ``http://127.0.0.1:<port>/acp``. Override ``hostname`` for a
    remote daemon (``--hostname`` from the Kilo CLI) or ``scheme`` for TLS.
    """
    host = (hostname or "127.0.0.1").strip()
    if not host:
        host = "127.0.0.1"
    return f"{scheme}://{host}:{int(port)}{path}"


def _resolve_kilo_auth_headers() -> dict[str, str]:
    """Map Kilo auth state to HTTP headers — graceful when unauthenticated.

    Kilo uses ``kilo auth login`` to create a local credential that ``kilo acp``
    reads. The HTTP server may also accept a plain ``KILO_API_KEY`` bearer.
    We never raise here: an unauthenticated worker is allowed to start; the
    server surfaces 401/403 on the first protected RPC.
    """
    token = (
        os.environ.get("KILO_API_KEY")
        or os.environ.get("KILO_AUTH_TOKEN")
        or ""
    ).strip()
    if token:
        return {"Authorization": f"Bearer {token}"}
    return {}


class KiloAcpBackend:
    """Long-lived ACP client backed by ``kilo acp --port``.

    Usage::

        backend = KiloAcpBackend(port=8721, hostname="127.0.0.1",
                                 cwd="/path/to/repo", model="anthropic/claude-sonnet-4")
        text, ctx = await backend.run("What is 2+2?", {})
        text, ctx = await backend.run("And 3+3?", ctx)   # resume
        await backend.close()

    The backend is a thin wrapper around :class:`AcpHttpBackend`; everything
    protocol-shaped lives there. We only add Kilo-specific auth, URL
    construction, session-id namespacing, and a best-effort model override.
    """

    def __init__(
        self,
        *,
        port: int | str = DEFAULT_KILO_PORT,
        hostname: str = "127.0.0.1",
        cwd: str | None = None,
        model: str | None = None,
        auth_headers: dict[str, str] | None = None,
        log_label: str = "kilo-acp-http",
        request_timeout_sec: float = 900.0,
        always_approve: bool = True,
        http2: bool = True,
        base_url: str | None = None,
    ) -> None:
        self.port = port
        self.hostname = hostname
        self.cwd = cwd
        self.model = model
        self.log_label = log_label
        self.request_timeout_sec = request_timeout_sec
        self.always_approve = always_approve
        self.http2 = http2
        self.base_url = base_url or resolve_kilo_url(port, hostname)

        # Merge caller-supplied headers with Kilo env-driven auth. Caller wins.
        merged: dict[str, str] = {}
        merged.update(_resolve_kilo_auth_headers())
        if auth_headers:
            merged.update(auth_headers)
        self.auth_headers = merged

        self._inner = AcpHttpBackend(
            base_url=self.base_url,
            cwd=cwd,
            model=model,
            auth_headers=self.auth_headers or None,
            log_label=log_label,
            request_timeout_sec=request_timeout_sec,
            always_approve=always_approve,
            http2=http2,
        )
        self._lock_name = f"kilo-acp:{self.base_url}"

    # ── Delegated transport handle (read-only for callers) ──────

    @property
    def transport(self):
        """The underlying :class:`AcpHttpTransport` (for diagnostics / tests)."""
        return self._inner.transport

    # ── Lifecycle ───────────────────────────────────────────────

    async def start(self) -> None:
        """Open the HTTP client, run ``initialize``, open SSE streams."""
        await self._inner.start()

    async def close(self) -> None:
        """Tear down SSE streams, drain pending futures, close HTTP client."""
        await self._inner.close()

    # ── Progress handling ───────────────────────────────────────

    def set_progress_handler(self, handler) -> None:
        """``async def handler(kind, data)`` — ``kind`` ∈ {message,thought,tool}."""
        self._inner.set_progress_handler(handler)

    def clear_progress_handler(self) -> None:
        self._inner.clear_progress_handler()

    # ── Core WorkerBackend.run ─────────────────────────────────

    async def run(self, prompt: str,
                  ctx: dict[str, Any] | None) -> tuple[str, dict[str, Any]]:
        """Run a prompt against the Kilo ACP server.

        ``ctx`` carries per-session state. We persist the Kilo session id under
        both ``kilo_acp_session_id`` (canonical) and ``acp_session_id`` (legacy
        parity) so other backends can swap in without rewriting keys.
        """
        ctx = dict(ctx or {})

        # Prefer a Kilo-namespaced session id, fall back to legacy.
        existing = ctx.get(SESSION_CTX_KEY) or ctx.get(SESSION_CTX_KEY_LEGACY)
        ctx[SESSION_CTX_KEY_LEGACY] = existing  # keep legacy key warm

        # Run via the generic backend. It handles initialize/session-new/
        # session-prompt lifecycle and streams agent_message_chunk into text.
        text, new_ctx = await self._inner.run(prompt, ctx)

        # Mirror the session id back to the Kilo-namespaced key.
        sid = new_ctx.get(SESSION_CTX_KEY_LEGACY) or new_ctx.get(SESSION_CTX_KEY)
        if sid:
            new_ctx[SESSION_CTX_KEY] = sid
            new_ctx[SESSION_CTX_KEY_LEGACY] = sid

        # Best-effort late model binding — some Kilo versions accept ``model``
        # only via session/set_model after session/new rather than inline.
        if self.model and sid:
            try:
                await self._inner.transport.request(
                    "session/set_model",
                    {"sessionId": sid, "model": self.model},
                    session_id=sid,
                    timeout=30.0,
                )
                logger.info("[%s] model set to %s", self.log_label, self.model)
            except AcpHttpError as e:
                logger.debug("[%s] session/set_model ignored: %s", self.log_label, e)

        return text, new_ctx
