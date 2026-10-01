"""
Hermes ACP backend — JSON-RPC 2.0 over stdio to `hermes acp`.

Built on the shared ``AcpStdioBackend`` (the same plumbing as grok/opencode),
so it needs no extra Python package, streams progress, restarts a dead agent,
answers permission requests by policy, and supports cancel via ACP
``session/cancel``.

Hermes advertises ``loadSession``: a session id from an earlier `hermes acp`
process (worker restart, or a persisted ``backend_ctx``) is reattached with
``session/load`` instead of starting over.
"""

from __future__ import annotations

import logging
import os
from typing import Any

from worker_backends.acp_stdio import AcpStdioBackend

logger = logging.getLogger(__name__)


class HermesAcpBackend(AcpStdioBackend):
    """
    Long-lived ACP client for Hermes Agent.

    Usage::

        backend = HermesAcpBackend(model="claude-sonnet-4")
        text, ctx = await backend.run("What is 2+2?", {})
        text, ctx = await backend.run("And 3+3?", ctx)
        await backend.close()
    """

    label = "Hermes ACP"
    session_ctx_key = "acp_session_id"

    def __init__(
        self,
        model: str | None = None,
        cwd: str | None = None,
        hermes_cmd: str = "hermes",
        acp_args: list[str] | None = None,
        log_label: str = "hermes-acp",
        request_timeout_sec: float = 900.0,
        permission_policy: str = "allow_always",
    ) -> None:
        super().__init__(
            cwd=cwd or os.getcwd(),
            log_label=log_label,
            request_timeout_sec=request_timeout_sec,
            permission_policy=permission_policy,
        )
        self.model = model
        self.hermes_cmd = hermes_cmd
        self.acp_args = list(acp_args or [])
        self._can_load = False

    def _command(self) -> list[str]:
        return [self.hermes_cmd, "acp", *self.acp_args]

    async def _authenticate(self, init: dict[str, Any]) -> None:
        caps = init.get("agentCapabilities") or {}
        self._can_load = bool(caps.get("loadSession"))
        # Hermes uses its configured runtime credentials; only a non-terminal
        # method can be run headless. Failure is not fatal (the first prompt
        # reports a real auth problem with Hermes' own message).
        methods = [m for m in init.get("authMethods") or []
                   if isinstance(m, dict) and m.get("id") and m.get("type") != "terminal"]
        if not methods:
            return
        try:
            await self._request("authenticate", {"methodId": str(methods[0]["id"])}, timeout=30.0)
        except Exception as e:  # noqa: BLE001
            logger.debug("[%s] authenticate ignored: %s", self.log_label, e)

    async def _new_session(self) -> str:
        session_id = await super()._new_session()
        await self._set_model(session_id)
        return session_id

    async def _load_session(self, session_id: str) -> bool:
        if not self._can_load:
            return False
        try:
            await self._request("session/load", {
                "sessionId": session_id, "cwd": self.cwd, "mcpServers": [],
            }, timeout=60.0)
        except Exception as e:  # noqa: BLE001 - expired/unknown: start a new one
            logger.warning("[%s] session/load failed (%s); new session", self.log_label, e)
            return False
        await self._set_model(session_id)
        return True

    async def _set_model(self, session_id: str) -> None:
        if not self.model:
            return
        try:
            await self._request("session/set_model",
                                {"sessionId": session_id, "modelId": self.model}, timeout=30.0)
        except Exception as e:  # noqa: BLE001 - not supported on every version
            logger.debug("[%s] set_model ignored: %s", self.log_label, e)


__all__ = ["HermesAcpBackend"]
