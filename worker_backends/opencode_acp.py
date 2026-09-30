"""OpenCode ACP backend — JSON-RPC 2.0 over stdio to `opencode acp`.

Lifecycle: initialize → authenticate (optional) → session/new → session/prompt.
Assistant text streams as session/update agent_message_chunk notifications.

Raw JSON-RPC (no `acp` Python library); the shared plumbing (stderr drain,
permission choice by option kind, restart on EOF) is in
``worker_backends/acp_stdio.py``.
"""

from __future__ import annotations

import logging
import os
from pathlib import Path
from typing import Any

from worker_backends.acp_stdio import AcpStdioBackend

logger = logging.getLogger(__name__)


def resolve_opencode_bin() -> str:
    """Locate the `opencode` binary; honors $OPENCODE_BIN, falls back to PATH."""
    env = os.environ.get("OPENCODE_BIN")
    if env and Path(env).exists():
        return env
    for p in (
        Path.home() / ".local/bin/opencode",
        Path("/usr/local/bin/opencode"),
        Path.home() / ".opencode/bin/opencode",
    ):
        if p.exists() and os.access(p, os.X_OK):
            return str(p)
    return "opencode"


def _self_meta(provider: str | None) -> dict[str, Any]:
    if provider:
        return {"_meta": {"opencode": {"provider": provider}}}
    return {}


class OpencodeAcpBackend(AcpStdioBackend):
    """Long-lived ACP client for the OpenCode CLI.

        backend = OpencodeAcpBackend(model="anthropic/claude-sonnet-4")
        text, ctx = await backend.run("Say hi", {})
        text, ctx = await backend.run("Continue", ctx)

    Sessions survive across turns via ctx["opencode_acp_session_id"] (for as
    long as the same `opencode acp` process lives).
    """

    label = "OpenCode ACP"
    session_ctx_key = "opencode_acp_session_id"

    def __init__(
        self,
        *,
        cwd: str | None = None,
        opencode_cmd: str | None = None,
        model: str | None = None,
        provider: str | None = None,
        always_approve: bool = True,
        permission_policy: str | None = None,
        log_label: str = "opencode-acp",
        request_timeout_sec: float = 900.0,
        acp_args: list[str] | None = None,
    ) -> None:
        super().__init__(
            cwd=cwd or os.getcwd(),
            log_label=log_label,
            request_timeout_sec=request_timeout_sec,
            permission_policy=permission_policy
            or ("allow_always" if always_approve else "reject"),
        )
        self.opencode_cmd = opencode_cmd or resolve_opencode_bin()
        self.model = model
        self.provider = provider
        self.always_approve = always_approve
        self.acp_args = list(acp_args or [])

    def _command(self) -> list[str]:
        return [self.opencode_cmd, "acp", *self.acp_args]

    @staticmethod
    def _any_provider_env() -> bool:
        return any(os.environ.get(v) for v in (
            "ANTHROPIC_API_KEY", "OPENAI_API_KEY",
            "GOOGLE_API_KEY", "GEMINI_API_KEY", "OPENCODE_API_KEY",
        ))

    async def _authenticate(self, init: dict[str, Any]) -> None:
        auth_methods = {
            str(m.get("id"))
            for m in (init.get("authMethods") or [])
            if isinstance(m, dict) and m.get("id")
        }
        if not auth_methods:
            return
        if "env" in auth_methods and self._any_provider_env():
            chosen = "env"
        else:
            default_auth = (init.get("_meta") or {}).get("defaultAuthMethodId")
            chosen = default_auth if default_auth in auth_methods else sorted(auth_methods)[0]
        try:
            await self._request("authenticate", {"methodId": chosen, "_meta": {"headless": True}})
            logger.info("[%s] authenticated via %s", self.log_label, chosen)
        except Exception as e:
            logger.debug("[%s] authenticate(%s) ignored: %s", self.log_label, chosen, e)

    async def _set_model(self, session_id: str) -> bool:
        try:
            await self._request("session/set_model", {
                "sessionId": session_id, "modelId": self.model, **_self_meta(self.provider),
            }, timeout=30.0)
            return True
        except Exception as e:
            logger.debug("[%s] set_model ignored: %s", self.log_label, e)
            return False

    async def _new_session(self) -> str:
        new = await self._request("session/new", {
            "cwd": self.cwd, "mcpServers": [], **_self_meta(self.provider),
        })
        session_id = (new or {}).get("sessionId")
        if not session_id:
            raise RuntimeError(f"session/new missing sessionId: {new}")
        if self.model:
            await self._set_model(session_id)
        if self.always_approve:
            try:
                await self._request("session/set_mode",
                                    {"sessionId": session_id, "modeId": "always-allow"}, timeout=15.0)
            except Exception:
                pass
        return str(session_id)

    async def _on_resume(self, session_id: str, ctx: dict[str, Any]) -> None:
        if self.model and self.model != ctx.get("opencode_acp_model"):
            if await self._set_model(session_id):
                ctx["opencode_acp_model"] = self.model


__all__ = ["OpencodeAcpBackend", "resolve_opencode_bin"]
