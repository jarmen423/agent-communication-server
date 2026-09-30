"""
Grok ACP backend — JSON-RPC 2.0 over stdio to `grok agent stdio`.

Protocol (from Grok docs / live probe):
  initialize → authenticate → session/new → session/prompt
  Assistant text arrives as session/update agent_message_chunk notifications.

The JSON-RPC plumbing, stderr draining, permission handling and restart on
EOF live in ``worker_backends/acp_stdio.py``. This is intentionally separate
from HermesAcpBackend: Grok uses `session/*` method names and content-block
prompts, not Hermes' `sessions/*` + PromptRequest shape.
"""

from __future__ import annotations

import logging
import os
from pathlib import Path
from typing import Any

from worker_backends.acp_stdio import AcpStdioBackend

logger = logging.getLogger(__name__)


def resolve_grok_bin() -> str:
    env = os.environ.get("GROK_BIN")
    if env and Path(env).exists():
        return env
    candidates: list[Path] = [
        Path.home() / ".local/bin/grok",
        Path("/usr/local/bin/grok"),
    ]
    downloads = Path.home() / ".grok/downloads"
    if downloads.is_dir():
        candidates = sorted(downloads.glob("grok-*-linux-x86_64"), reverse=True) + candidates
    for p in candidates:
        if p.exists() and os.access(p, os.X_OK):
            return str(p)
    return "grok"


class GrokAcpBackend(AcpStdioBackend):
    """
    Long-lived ACP client for Grok Build TUI.

    Usage::

        backend = GrokAcpBackend(cwd="/path/to/repo")
        text, ctx = await backend.run("Say hi", {})
        text, ctx = await backend.run("Continue", ctx)  # same session
        await backend.close()

    ``permission_policy`` (allow_once | allow_always | reject) decides how
    ``session/request_permission`` is answered; when omitted it follows
    ``always_approve`` (allow_always / reject).
    """

    label = "Grok ACP"
    session_ctx_key = "grok_session_id"

    def __init__(
        self,
        *,
        cwd: str | None = None,
        grok_cmd: str | None = None,
        model: str | None = None,
        always_approve: bool = True,
        permission_policy: str | None = None,
        log_label: str = "grok-acp",
        request_timeout_sec: float = 900.0,
        no_auto_update: bool = True,
    ) -> None:
        super().__init__(
            cwd=cwd or os.getcwd(),
            log_label=log_label,
            request_timeout_sec=request_timeout_sec,
            permission_policy=permission_policy
            or ("allow_always" if always_approve else "reject"),
        )
        self.grok_cmd = grok_cmd or resolve_grok_bin()
        self.model = model
        self.always_approve = always_approve
        self.no_auto_update = no_auto_update

    def _command(self) -> list[str]:
        if self.no_auto_update:
            return [self.grok_cmd, "--no-auto-update", "agent", "stdio"]
        return [self.grok_cmd, "agent", "stdio"]

    async def _authenticate(self, init: dict[str, Any]) -> None:
        auth_methods = {
            str(m.get("id"))
            for m in (init.get("authMethods") or [])
            if isinstance(m, dict) and m.get("id")
        }
        default_auth = (init.get("_meta") or {}).get("defaultAuthMethodId")
        if os.environ.get("XAI_API_KEY") and "xai.api_key" in auth_methods:
            method_id = "xai.api_key"
        elif isinstance(default_auth, str) and default_auth in auth_methods:
            method_id = default_auth
        elif "cached_token" in auth_methods:
            method_id = "cached_token"
        else:
            raise RuntimeError(
                "Grok ACP has no usable auth method. Run `grok login` or set XAI_API_KEY. "
                f"authMethods={sorted(auth_methods)}"
            )
        await self._request("authenticate", {"methodId": method_id, "_meta": {"headless": True}})
        logger.info("[%s] authenticated via %s", self.log_label, method_id)

    async def _new_session(self) -> str:
        session_id = await super()._new_session()
        if self.model:
            try:
                await self._request("session/set_model",
                                    {"sessionId": session_id, "model": self.model}, timeout=30.0)
            except Exception as e:
                logger.debug("[%s] set_model ignored: %s", self.log_label, e)
        if self.always_approve:
            try:
                await self._request("session/command", {
                    "sessionId": session_id, "name": "always-approve", "input": "on",
                }, timeout=15.0)
            except Exception:
                pass
        return session_id


__all__ = ["GrokAcpBackend", "resolve_grok_bin"]
