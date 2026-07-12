"""Headless CLI: non-interactive subprocess agents (agy -p, hermes chat -q, etc.)."""

from __future__ import annotations

import asyncio
import re
import shlex
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable


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
    timeout_sec: float | None = None


class HeadlessCliBackend:
    def __init__(self, spec: HeadlessCliSpec) -> None:
        self.spec = spec
        self.repo = Path(spec.repo).resolve()

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
        import json as _json

        s = self.spec
        text_parts: list[str] = []
        session_id: str | None = None

        for line in raw.strip().splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                event = _json.loads(line)
            except (ValueError, TypeError):
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

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        cmd = self._build_cmd(prompt, ctx)
        cwd = str(self._cwd(ctx))
        print(
            f"[{self.spec.log_label}] exec: {' '.join(shlex.quote(c) for c in cmd[:8])}"
            f"{' ...' if len(cmd) > 8 else ''} cwd={cwd}"
        )

        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=cwd,
        )
        try:
            stdout, stderr = await asyncio.wait_for(
                proc.communicate(),
                timeout=self.spec.timeout_sec,
            )
        except asyncio.TimeoutError:
            proc.kill()
            raise RuntimeError(f"{self.spec.binary} timed out after {self.spec.timeout_sec}s")

        raw = stdout.decode().strip()
        if proc.returncode != 0:
            if raw:
                return self._parse_text(raw, ctx)
            raise RuntimeError(
                f"{self.spec.binary} exit {proc.returncode}: {stderr.decode().strip()[:800]}"
            )
        return self._parse_text(raw, ctx)