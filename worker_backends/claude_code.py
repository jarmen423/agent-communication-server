"""Claude Code backend — `claude -p --output-format stream-json --verbose`.

One subprocess per turn. Session turns resume with ``--resume <session_id>``,
where the id is captured from the stream (``system/init`` or ``result``).

stream-json events (one JSON object per line) we understand:
  {"type":"system","subtype":"init","session_id":…,"model":…}
  {"type":"assistant","message":{"content":[{"type":"text"|"thinking"|"tool_use",…}]}}
  {"type":"result","subtype":"success"|"error_…","is_error":bool,"result":str,"session_id":…}

Safety defaults: ``--permission-mode acceptEdits``. ``bypassPermissions`` is
refused unless the caller passes ``dangerously_skip_permissions=True``.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from worker_backends.headless_cli import (
    DEFAULT_TIMEOUT_SEC,
    CliTurn,
    HeadlessCliBackend,
    HeadlessCliSpec,
    ProgressHandler,
)

PERMISSION_MODES = ("acceptEdits", "auto", "bypassPermissions", "manual", "dontAsk", "plan")
DEFAULT_PERMISSION_MODE = "acceptEdits"
SESSION_CTX_KEY = "claude_session_id"
_MAX_PROGRESS_CHARS = 2000


@dataclass
class ClaudeCodeConfig:
    claude_bin: str = "claude"
    repo: str | Path = "."
    model: str | None = None
    permission_mode: str = DEFAULT_PERMISSION_MODE
    dangerously_skip_permissions: bool = False
    allowed_tools: list[str] = field(default_factory=list)
    extra_args: list[str] = field(default_factory=list)
    timeout_sec: float | None = DEFAULT_TIMEOUT_SEC


def resolve_permission_mode(mode: str | None, dangerously_skip_permissions: bool) -> str:
    """Validate the permission mode; bypass needs the explicit dangerous opt-in."""
    if dangerously_skip_permissions:
        return "bypassPermissions"
    mode = mode or DEFAULT_PERMISSION_MODE
    if mode not in PERMISSION_MODES:
        raise ValueError(f"unknown permission mode {mode!r}; choose from {PERMISSION_MODES}")
    if mode == "bypassPermissions":
        raise ValueError(
            "permission mode bypassPermissions requires --dangerously-skip-permissions"
        )
    return mode


def _clip(text: str) -> str:
    return text if len(text) <= _MAX_PROGRESS_CHARS else text[:_MAX_PROGRESS_CHARS] + "…"


class ClaudeTurn(CliTurn):
    """Parses one turn of stream-json output."""

    def __init__(self) -> None:
        self.session_id: str | None = None
        self.texts: list[str] = []
        self.result: dict[str, Any] | None = None

    async def feed(self, line: str, emit: ProgressHandler) -> None:
        line = line.strip()
        if not line:
            return
        try:
            event = json.loads(line)
        except ValueError:
            return
        if not isinstance(event, dict):
            return
        sid = event.get("session_id")
        if isinstance(sid, str) and sid:
            self.session_id = sid
        etype = event.get("type")
        if etype == "system" and event.get("subtype") == "init":
            model = event.get("model") or "?"
            await emit("status", {"message": f"claude session started (model={model})",
                                  "session_id": self.session_id, "model": model})
        elif etype == "assistant":
            await self._on_assistant(event, emit)
        elif etype == "result":
            self.result = event

    async def _on_assistant(self, event: dict[str, Any], emit: ProgressHandler) -> None:
        message = event.get("message") or {}
        content = message.get("content") if isinstance(message, dict) else None
        if not isinstance(content, list):
            return
        for block in content:
            if not isinstance(block, dict):
                continue
            btype = block.get("type")
            if btype == "text" and block.get("text"):
                self.texts.append(str(block["text"]))
                await emit("message", {"text": _clip(str(block["text"]))})
            elif btype == "thinking" and block.get("thinking"):
                await emit("thought", {"text": _clip(str(block["thinking"]))})
            elif btype == "tool_use":
                name = block.get("name") or "tool"
                await emit("tool", {"message": f"tool: {name}", "tool": name})

    def _result_error(self) -> str | None:
        r = self.result
        if r is None:
            return None
        if not r.get("is_error") and r.get("subtype", "success") == "success":
            return None
        detail = r.get("result") or r.get("error") or r.get("errors") or ""
        if not isinstance(detail, str):
            detail = json.dumps(detail)
        return f"claude {r.get('subtype') or 'error'}: {detail}".strip().rstrip(":")

    def error_hint(self) -> str | None:
        return self._result_error()

    def finish(self, raw: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        err = self._result_error()
        if err:
            raise RuntimeError(err)
        text = ""
        if self.result is not None:
            res = self.result.get("result")
            text = res.strip() if isinstance(res, str) else ""
        if not text and self.texts:
            text = self.texts[-1].strip()
        if not text:
            if self.result is None:
                raise RuntimeError("claude exited without a result event (stream-json)")
            text = "(claude returned no text)"
        ctx = dict(ctx)
        if self.session_id:
            ctx[SESSION_CTX_KEY] = self.session_id
        return text, ctx


class ClaudeCodeBackend(HeadlessCliBackend):
    """WorkerBackend for Claude Code in print mode with stream-json output."""

    def __init__(self, cfg: ClaudeCodeConfig) -> None:
        self.cfg = cfg
        self.permission_mode = resolve_permission_mode(
            cfg.permission_mode, cfg.dangerously_skip_permissions
        )
        base = ["-p", "--output-format", "stream-json", "--verbose",
                "--permission-mode", self.permission_mode]
        if cfg.model:
            base.extend(["--model", cfg.model])
        if cfg.allowed_tools:
            # `=` form: --allowed-tools is variadic and would swallow later args.
            base.append("--allowed-tools=" + ",".join(cfg.allowed_tools))
        base.extend(cfg.extra_args)
        super().__init__(
            HeadlessCliSpec(
                binary=cfg.claude_bin,
                log_label="claude-worker",
                repo=cfg.repo,
                base_argv=base,
                prompt_flag=None,
                end_of_options=True,
                resume_mode="resume_id",
                resume_id_flag="--resume",
                resume_ctx_key=SESSION_CTX_KEY,
                timeout_sec=cfg.timeout_sec,
            )
        )

    def _make_turn(self, prompt: str, ctx: dict[str, Any]) -> ClaudeTurn:
        return ClaudeTurn()
