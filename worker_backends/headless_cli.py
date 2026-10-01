"""Headless CLI: non-interactive subprocess agents (agy -p, hermes chat -q, etc.).

Hardening guarantees (see tests/python/test_worker_headless.py):
  - every turn has a timeout (default 900s); on timeout the child's whole
    process group is killed and reaped before the error is raised;
  - cancelling the turn's task (hub cancel, refocus-iteration-2.md §4.2)
    kills and reaps the process group the same way;
  - a non-zero exit is always an error, with a stderr tail in the message;
  - stderr is drained concurrently (bounded), stdout is streamed line by line;
  - positional prompts can be preceded by ``--`` so a prompt that starts
    with ``-`` is never parsed as a flag.

Streaming CLIs (Claude Code stream-json, Codex JSONL) subclass
``HeadlessCliBackend`` and return a ``CliTurn`` from ``_make_turn`` that turns
stdout lines into progress events and builds the final text.
"""

from __future__ import annotations

import json
import re
import shlex
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Awaitable, Callable

from worker_backends.proc import ProcResult, ProcTimeout, run_streaming
from worker_backends.progress import current_progress_handler

DEFAULT_TIMEOUT_SEC = 900.0

# per-turn progress handler (worker_backends.progress): async (kind, data) -> None
ProgressHandler = Callable[[str, dict[str, Any]], Awaitable[None]]


@dataclass
class HeadlessCliSpec:
    """Declarative headless CLI backend."""

    binary: str
    log_label: str = "cli"
    repo: str | Path = "."
    # Fixed argv before prompt (e.g. ["hermes", "chat", "-Q"])
    base_argv: list[str] = field(default_factory=list)
    # Prompt: append ["-p", prompt] or ["-q", prompt]; None = positional arg
    prompt_flag: str | None = "-p"
    # Insert "--" before a positional prompt (only when prompt_flag is None)
    end_of_options: bool = False
    # Session resume
    resume_mode: str = "none"  # none | continue_flag | resume_id | session_cwd_continue
    continue_flag: str = "--continue"
    resume_id_flag: str = "--resume"
    resume_ctx_key: str = "session_id"
    has_turn_ctx_key: str = "has_turn"
    # Per hub-session cwd: repo / subdir / ctx["_session_id"]
    session_cwd_subdir: str | None = None
    # Output
    parse_session_id: Callable[[str], str | None] | None = None
    strip_line_prefixes: tuple[str, ...] = ()
    # When True, parse NDJSON event stream and extract text + session ID
    json_events: bool = False
    json_text_key: str = "text"       # key inside event["part"] for text content
    json_session_key: str = "sessionID"  # key inside event for session ID
    # Per-turn wall-clock limit. None disables it (not recommended).
    timeout_sec: float | None = DEFAULT_TIMEOUT_SEC
    env: dict[str, str] | None = None


class CliTurn:
    """Per-turn stdout parser for streaming CLIs. Default: collect only."""

    async def feed(self, line: str, emit: ProgressHandler) -> None:  # noqa: B027
        """Handle one stdout line; call ``emit(kind, data)`` for progress."""

    def error_hint(self) -> str | None:
        """Best error text parsed from stdout (used on non-zero exit)."""
        return None

    def finish(self, raw: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        raise NotImplementedError

    def cleanup(self) -> None:  # noqa: B027
        """Release per-turn resources (temp files)."""


class HeadlessCliBackend:
    def __init__(self, spec: HeadlessCliSpec) -> None:
        self.spec = spec
        self.repo = Path(spec.repo).resolve()
        self._progress_handler: ProgressHandler | None = None

    # Fallback handler for direct callers/tests. The runtime passes a per-turn
    # handler through worker_backends.progress instead (no cross-talk).
    def set_progress_handler(self, handler: ProgressHandler | None) -> None:
        self._progress_handler = handler

    def clear_progress_handler(self) -> None:
        self._progress_handler = None

    def _cwd(self, ctx: dict[str, Any]) -> Path:
        sid = ctx.get("_session_id")
        if sid and self.spec.session_cwd_subdir:
            p = self.repo / self.spec.session_cwd_subdir / sid
            p.mkdir(parents=True, exist_ok=True)
            return p
        return self.repo

    def _build_cmd(self, prompt: str, ctx: dict[str, Any]) -> list[str]:
        s = self.spec
        cmd = [s.binary, *s.base_argv]

        if s.resume_mode == "continue_flag" and ctx.get(s.has_turn_ctx_key):
            cmd.append(s.continue_flag)
        elif s.resume_mode == "resume_id" and ctx.get(s.resume_ctx_key):
            cmd.extend([s.resume_id_flag, str(ctx[s.resume_ctx_key])])
        elif s.resume_mode == "session_cwd_continue" and ctx.get(s.has_turn_ctx_key):
            cmd.append(s.continue_flag)

        if s.prompt_flag:
            cmd.extend([s.prompt_flag, prompt])
        else:
            if s.end_of_options:
                cmd.append("--")
            cmd.append(prompt)
        return cmd

    def _parse_text(self, raw: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        s = self.spec
        ctx = dict(ctx or {})

        # NDJSON event stream mode (kilo --format json, opencode --format json)
        if s.json_events:
            return self._parse_json_events(raw, ctx)

        lines = raw.strip().splitlines()
        sid = None
        if s.parse_session_id:
            sid = s.parse_session_id(raw)
        for line in lines:
            if line.startswith("session_id:"):
                sid = line.split(":", 1)[1].strip()
                break
        if not sid and s.resume_mode == "resume_id":
            m = re.search(r"session[_\s-]?id[:\s]+(\S+)", raw, re.I)
            if m:
                sid = m.group(1)

        out_lines = [
            ln
            for ln in lines
            if not any(ln.startswith(p) for p in s.strip_line_prefixes)
        ]
        text = "\n".join(out_lines).strip() or raw.strip()

        if sid and s.resume_ctx_key:
            ctx[s.resume_ctx_key] = sid
        if s.resume_mode in ("continue_flag", "session_cwd_continue"):
            ctx[s.has_turn_ctx_key] = True
        return text, ctx

    def _parse_json_events(self, raw: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        """Parse NDJSON event stream from kilo/opencode --format json output.

        Extracts text content from text-type events and session ID.
        Returns concatenated text and updated ctx with session ID.
        """
        s = self.spec
        text_parts: list[str] = []
        session_id: str | None = None

        for line in raw.strip().splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except (ValueError, TypeError):
                continue
            if not isinstance(event, dict):
                continue

            # Capture session ID from any event that has it
            if not session_id:
                session_id = event.get(s.json_session_key) or event.get("sessionID")

            # Error events — surface the error message
            if event.get("type") == "error":
                err = event.get("error", {})
                msg = err.get("message") if isinstance(err, dict) else str(err)
                if msg:
                    text_parts.append(f"[error: {msg}]")
                continue

            # Text events — extract the text content
            part = event.get("part", {})
            if isinstance(part, dict) and part.get("type") == "text":
                t = part.get(s.json_text_key)
                if t:
                    text_parts.append(t)

        text = "".join(text_parts).strip() or raw.strip()

        if session_id and s.resume_ctx_key:
            ctx[s.resume_ctx_key] = session_id
        if s.resume_mode in ("continue_flag", "session_cwd_continue"):
            ctx[s.has_turn_ctx_key] = True

        return text, ctx

    # ── Turn hooks (overridden by streaming backends) ──────────────────

    def _make_turn(self, prompt: str, ctx: dict[str, Any]) -> CliTurn | None:
        return None

    def _cmd_for_turn(self, prompt: str, ctx: dict[str, Any], turn: CliTurn | None) -> list[str]:
        return self._build_cmd(prompt, ctx)

    # ── Execution ──────────────────────────────────────────────────────

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        ctx = dict(ctx or {})
        turn = self._make_turn(prompt, ctx)
        try:
            cmd = self._cmd_for_turn(prompt, ctx, turn)
            cwd = str(self._cwd(ctx))
            print(
                f"[{self.spec.log_label}] exec: {' '.join(shlex.quote(c) for c in cmd[:8])}"
                f"{' ...' if len(cmd) > 8 else ''} cwd={cwd}",
                flush=True,
            )
            handler = current_progress_handler(self._progress_handler)

            async def emit(kind: str, data: dict[str, Any]) -> None:
                if handler is None:
                    return
                try:
                    await handler(kind, data)
                except Exception as e:  # progress must never break a turn
                    print(f"[{self.spec.log_label}] progress handler error: {e}", flush=True)

            on_line = None
            if turn is not None:
                async def on_line(line: str) -> None:
                    await turn.feed(line, emit)

            try:
                result = await run_streaming(
                    cmd, cwd=cwd, timeout=self.spec.timeout_sec, on_line=on_line, env=self.spec.env
                )
            except ProcTimeout as e:
                raise RuntimeError(
                    f"{self.spec.binary} timed out after {e.timeout:g}s (process group killed)"
                    + (f"; stderr: {e.stderr_tail}" if e.stderr_tail else "")
                ) from None

            if result.returncode != 0:
                raise RuntimeError(self._exit_error(result, turn))
            if turn is not None:
                return turn.finish(result.stdout, ctx)
            return self._parse_text(result.stdout.strip(), ctx)
        finally:
            if turn is not None:
                turn.cleanup()

    def _exit_error(self, result: ProcResult, turn: CliTurn | None) -> str:
        details: list[str] = []
        hint = turn.error_hint() if turn is not None else None
        if hint:
            details.append(hint[:800])
        if result.stderr_tail:
            details.append(f"stderr: {result.stderr_tail[-800:]}")
        elif not hint and result.stdout.strip():
            details.append(f"stdout: {result.stdout.strip()[-400:]}")
        head = f"{self.spec.binary} exit {result.returncode}"
        return f"{head} — {' | '.join(details)}" if details else head
