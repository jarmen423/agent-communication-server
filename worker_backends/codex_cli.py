"""Codex backend — `codex exec --json` (JSONL events), one subprocess per turn.

First turn:   codex exec --json -C <repo> -s <sandbox> [-m M] -o <file> -- <prompt>
Session turn: codex exec resume --json -c sandbox_mode="<sandbox>" [-m M] -o <file> -- <thread_id> <prompt>
(`exec resume` accepts neither -C nor -s, so the process cwd is the repo and
the sandbox goes through a config override.)

JSONL events we understand:
  {"type":"thread.started","thread_id":…}
  {"type":"item.started"|"item.updated"|"item.completed","item":{"type":"agent_message"|"reasoning"|"command_execution"|…}}
  {"type":"turn.completed","usage":…} / {"type":"turn.failed","error":{"message":…}}
  {"type":"error","message":…}
The final answer is the last completed ``agent_message``; ``-o`` (the
last-message file) is the fallback.

Safety defaults: sandbox ``workspace-write``. The bypass flag
``--dangerously-bypass-approvals-and-sandbox`` is only used when the caller
sets ``dangerously_bypass=True``.
"""

from __future__ import annotations

import json
import os
import tempfile
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

SANDBOX_MODES = ("read-only", "workspace-write", "danger-full-access")
DEFAULT_SANDBOX = "workspace-write"
SESSION_CTX_KEY = "codex_thread_id"
_MAX_PROGRESS_CHARS = 2000


@dataclass
class CodexConfig:
    codex_bin: str = "codex"
    repo: str | Path = "."
    model: str | None = None
    sandbox: str = DEFAULT_SANDBOX
    skip_git_repo_check: bool = False
    dangerously_bypass: bool = False
    extra_args: list[str] = field(default_factory=list)
    timeout_sec: float | None = DEFAULT_TIMEOUT_SEC


def _clip(text: str) -> str:
    return text if len(text) <= _MAX_PROGRESS_CHARS else text[:_MAX_PROGRESS_CHARS] + "…"


def _item_progress(item: dict[str, Any]) -> tuple[str, dict[str, Any]] | None:
    itype = item.get("type") or item.get("item_type")
    status = item.get("status") or ""
    if itype == "reasoning" and item.get("text"):
        return "thought", {"text": _clip(str(item["text"]))}
    if itype == "command_execution":
        msg = f"exec: {item.get('command') or '?'} {status}".strip()
        return "tool", {"message": _clip(msg), "tool": "command_execution"}
    if itype == "file_change":
        paths = [c.get("path") for c in item.get("changes") or [] if isinstance(c, dict)]
        return "tool", {"message": _clip(f"patch: {', '.join(p for p in paths if p)} {status}".strip()),
                        "tool": "file_change"}
    if itype == "mcp_tool_call":
        msg = f"mcp: {item.get('server') or '?'}.{item.get('tool') or '?'} {status}".strip()
        return "tool", {"message": msg, "tool": "mcp_tool_call"}
    if itype == "web_search":
        return "tool", {"message": f"web_search: {item.get('query') or ''}".strip(), "tool": "web_search"}
    return None


class CodexTurn(CliTurn):
    """Parses one `codex exec --json` turn."""

    def __init__(self) -> None:
        fd, path = tempfile.mkstemp(prefix="nats-hub-codex-", suffix=".txt")
        os.close(fd)
        self.last_message_file = path
        self.thread_id: str | None = None
        self.messages: list[str] = []
        self.errors: list[str] = []
        self.failed: str | None = None
        self.completed = False

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
        etype = event.get("type")
        if etype == "thread.started" and event.get("thread_id"):
            self.thread_id = str(event["thread_id"])
            await emit("status", {"message": "codex thread started", "thread_id": self.thread_id})
        elif etype in ("item.started", "item.updated", "item.completed"):
            item = event.get("item") or {}
            if not isinstance(item, dict):
                return
            if item.get("type") == "agent_message":
                if etype == "item.completed" and item.get("text"):
                    self.messages.append(str(item["text"]))
                    await emit("message", {"text": _clip(str(item["text"]))})
                return
            if item.get("type") == "error" and item.get("message"):
                self.errors.append(str(item["message"]))
                return
            if etype == "item.updated":
                return
            prog = _item_progress(item)
            if prog:
                await emit(*prog)
        elif etype == "turn.completed":
            self.completed = True
        elif etype == "turn.failed":
            err = event.get("error") or {}
            self.failed = (err.get("message") if isinstance(err, dict) else str(err)) or "turn failed"
        elif etype == "error" and event.get("message"):
            self.errors.append(str(event["message"]))

    def _read_last_message_file(self) -> str:
        try:
            return Path(self.last_message_file).read_text(encoding="utf-8").strip()
        except OSError:
            return ""

    def error_hint(self) -> str | None:
        if self.failed:
            return f"codex turn failed: {self.failed}"
        if self.errors:
            return f"codex error: {self.errors[-1]}"
        return None

    def finish(self, raw: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        if self.failed:
            raise RuntimeError(f"codex turn failed: {self.failed}")
        text = self.messages[-1].strip() if self.messages else ""
        if not text:
            text = self._read_last_message_file()
        if not text:
            hint = self.error_hint()
            raise RuntimeError(hint or "codex produced no final agent message")
        ctx = dict(ctx)
        if self.thread_id:
            ctx[SESSION_CTX_KEY] = self.thread_id
        return text, ctx

    def cleanup(self) -> None:
        try:
            os.unlink(self.last_message_file)
        except OSError:
            pass


class CodexBackend(HeadlessCliBackend):
    """WorkerBackend for `codex exec --json`, resuming by thread id."""

    def __init__(self, cfg: CodexConfig) -> None:
        if cfg.sandbox not in SANDBOX_MODES:
            raise ValueError(f"unknown sandbox {cfg.sandbox!r}; choose from {SANDBOX_MODES}")
        self.cfg = cfg
        super().__init__(
            HeadlessCliSpec(
                binary=cfg.codex_bin,
                log_label="codex-worker",
                repo=cfg.repo,
                prompt_flag=None,
                end_of_options=True,
                resume_mode="resume_id",
                resume_ctx_key=SESSION_CTX_KEY,
                timeout_sec=cfg.timeout_sec,
            )
        )

    def _make_turn(self, prompt: str, ctx: dict[str, Any]) -> CodexTurn:
        return CodexTurn()

    def _cmd_for_turn(self, prompt: str, ctx: dict[str, Any], turn: CliTurn | None) -> list[str]:
        cfg = self.cfg
        thread_id = ctx.get(SESSION_CTX_KEY)
        cmd = [cfg.codex_bin, "exec"]
        if thread_id:
            cmd.append("resume")
        cmd.append("--json")
        if not thread_id:
            cmd.extend(["-C", str(self.repo)])
        if cfg.skip_git_repo_check:
            cmd.append("--skip-git-repo-check")
        if cfg.dangerously_bypass:
            cmd.append("--dangerously-bypass-approvals-and-sandbox")
        elif thread_id:
            cmd.extend(["-c", f'sandbox_mode="{cfg.sandbox}"'])
        else:
            cmd.extend(["-s", cfg.sandbox])
        if cfg.model:
            cmd.extend(["-m", cfg.model])
        if isinstance(turn, CodexTurn):
            cmd.extend(["-o", turn.last_message_file])
        cmd.extend(cfg.extra_args)
        cmd.append("--")
        if thread_id:
            cmd.append(str(thread_id))
        cmd.append(prompt)
        return cmd
