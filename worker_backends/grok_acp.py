"""
Grok ACP backend — JSON-RPC 2.0 over stdio to `grok agent stdio`.

Protocol (from Grok docs / live probe):
  initialize → authenticate → session/new → session/prompt
  Assistant text arrives as session/update agent_message_chunk notifications.

This is intentionally separate from HermesAcpBackend: Grok uses
`session/*` method names and content-block prompts, not Hermes'
`sessions/*` + PromptRequest shape.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
from pathlib import Path
from typing import Any

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


class GrokAcpBackend:
    """
    Long-lived ACP client for Grok Build TUI.

    Usage::

        backend = GrokAcpBackend(cwd=\"/path/to/repo\")
        text, ctx = await backend.run(\"Say hi\", {})
        text, ctx = await backend.run(\"Continue\", ctx)  # same session
        await backend.close()
    """

    def __init__(
        self,
        *,
        cwd: str | None = None,
        grok_cmd: str | None = None,
        model: str | None = None,
        always_approve: bool = True,
        log_label: str = "grok-acp",
        request_timeout_sec: float = 900.0,
        no_auto_update: bool = True,
    ) -> None:
        self.cwd = str(Path(cwd or os.getcwd()).resolve())
        self.grok_cmd = grok_cmd or resolve_grok_bin()
        self.model = model
        self.always_approve = always_approve
        self.log_label = log_label
        self.request_timeout_sec = request_timeout_sec
        self.no_auto_update = no_auto_update

        self._proc: asyncio.subprocess.Process | None = None
        self._reader_task: asyncio.Task[None] | None = None
        self._pending: dict[int, asyncio.Future[Any]] = {}
        self._next_id = 1
        self._started = False
        self._chunks: list[str] = []
        self._lock = asyncio.Lock()
        self._progress_handler = None  # async (kind, data) -> None
        self._stream_buf: str = ""
        self._thought_buf: str = ""
        self._last_stream_emit: float = 0.0
        self._stream_min_interval: float = 0.35  # throttle hub events
        self._stream_min_chars: int = 40

    async def start(self) -> None:
        if self._started:
            return

        cmd = [self.grok_cmd, "agent", "stdio"]
        if self.no_auto_update:
            # Flag is accepted on top-level grok; also try as agent passthrough noise-safe.
            cmd = [self.grok_cmd, "--no-auto-update", "agent", "stdio"]

        logger.info("[%s] spawning %s (cwd=%s)", self.log_label, " ".join(cmd), self.cwd)
        self._proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=self.cwd,
        )
        self._reader_task = asyncio.create_task(self._read_loop())
        self._started = True

        init = await self._request(
            "initialize",
            {
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": {"readTextFile": True, "writeTextFile": True},
                    "terminal": True,
                },
            },
        )

        auth_methods = {
            str(m.get("id"))
            for m in (init.get("authMethods") or [])
            if isinstance(m, dict) and m.get("id")
        }
        meta = init.get("_meta") or {}
        default_auth = meta.get("defaultAuthMethodId")

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

        await self._request(
            "authenticate",
            {"methodId": method_id, "_meta": {"headless": True}},
        )
        logger.info("[%s] authenticated via %s", self.log_label, method_id)

    def set_progress_handler(self, handler) -> None:
        """Optional async callback: handler(kind: str, data: dict)."""
        self._progress_handler = handler

    def clear_progress_handler(self) -> None:
        self._progress_handler = None
        self._stream_buf = ""
        self._thought_buf = ""

    async def _emit_stream(self, kind: str, text: str, *, force: bool = False) -> None:
        """Throttle streaming hub publishes so we don't flood NATS."""
        if not self._progress_handler:
            return
        import time
        now = time.monotonic()
        if kind == "tool":
            try:
                await self._progress_handler("tool", {"text": text, "message": text})
            except Exception as e:
                logger.debug("[%s] progress handler error: %s", self.log_label, e)
            self._last_stream_emit = now
            return
        if kind == "thought":
            if text:
                self._thought_buf += text
            buf = self._thought_buf
        else:
            if text:
                self._stream_buf += text
            buf = self._stream_buf
        if not buf:
            return
        due = (now - self._last_stream_emit) >= self._stream_min_interval
        fat = len(buf) >= self._stream_min_chars
        if force or due or fat:
            snippet = buf[-800:]
            try:
                await self._progress_handler(
                    kind if kind in ("message", "thought") else "message",
                    {"text": snippet, "delta": text, "full_len": len(buf)},
                )
            except Exception as e:
                logger.debug("[%s] progress handler error: %s", self.log_label, e)
            self._last_stream_emit = now

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        async with self._lock:
            if not self._started:
                await self.start()

            session_id = ctx.get("acp_session_id") or ctx.get("grok_session_id")
            if not session_id:
                # Prefer a clean session for the real turn.
                new = await self._request(
                    "session/new",
                    {"cwd": self.cwd, "mcpServers": []},
                )
                session_id = new.get("sessionId")
                if not session_id:
                    raise RuntimeError(f"session/new missing sessionId: {new}")
                ctx = dict(ctx or {})
                ctx["acp_session_id"] = session_id
                ctx["grok_session_id"] = session_id
                logger.info("[%s] session %s", self.log_label, str(session_id)[:12])

                if self.model:
                    try:
                        await self._request(
                            "session/set_model",
                            {"sessionId": session_id, "model": self.model},
                            timeout=30.0,
                        )
                    except Exception as e:
                        logger.debug("[%s] set_model ignored: %s", self.log_label, e)

                if self.always_approve:
                    try:
                        await self._request(
                            "session/command",
                            {
                                "sessionId": session_id,
                                "name": "always-approve",
                                "input": "on",
                            },
                            timeout=15.0,
                        )
                    except Exception:
                        pass

            self._chunks.clear()
            self._stream_buf = ""
            self._thought_buf = ""
            self._last_stream_emit = 0.0
            logger.info("[%s] prompt: %s", self.log_label, prompt[:100])

            result = await self._request(
                "session/prompt",
                {
                    "sessionId": session_id,
                    "prompt": [{"type": "text", "text": prompt}],
                },
                timeout=self.request_timeout_sec,
            )

            # Flush any throttled tail so visualizer sees final partials.
            if self._stream_buf:
                await self._emit_stream("message", "", force=True)
            if self._thought_buf:
                await self._emit_stream("thought", "", force=True)

            # Collect stream text; fall back to any text fields in result.
            text = "".join(self._chunks).strip()
            if not text and isinstance(result, dict):
                text = (
                    result.get("text")
                    or result.get("result")
                    or result.get("message")
                    or ""
                )
                if isinstance(text, dict):
                    text = text.get("text") or json.dumps(text)
                text = str(text).strip()

            if not text:
                stop = result.get("stopReason") if isinstance(result, dict) else None
                text = f"(no text returned; stopReason={stop})"

            return text, ctx

    async def close(self) -> None:
        self._started = False
        if self._reader_task:
            self._reader_task.cancel()
            try:
                await self._reader_task
            except asyncio.CancelledError:
                pass
            self._reader_task = None

        for fut in self._pending.values():
            if not fut.done():
                fut.set_exception(RuntimeError("Grok ACP closed"))
        self._pending.clear()

        if self._proc:
            try:
                if self._proc.stdin:
                    self._proc.stdin.close()
                self._proc.terminate()
                try:
                    await asyncio.wait_for(self._proc.wait(), timeout=3)
                except asyncio.TimeoutError:
                    self._proc.kill()
            except ProcessLookupError:
                pass
            self._proc = None

    # ── JSON-RPC plumbing ────────────────────────────────────────────

    async def _request(
        self,
        method: str,
        params: dict[str, Any] | None = None,
        *,
        timeout: float | None = None,
    ) -> Any:
        if not self._proc or not self._proc.stdin:
            raise RuntimeError("Grok ACP process not started")

        req_id = self._next_id
        self._next_id += 1
        payload = {"jsonrpc": "2.0", "id": req_id, "method": method, "params": params or {}}
        loop = asyncio.get_running_loop()
        fut: asyncio.Future[Any] = loop.create_future()
        self._pending[req_id] = fut

        line = json.dumps(payload) + "\n"
        self._proc.stdin.write(line.encode("utf-8"))
        await self._proc.stdin.drain()

        try:
            return await asyncio.wait_for(fut, timeout=timeout or self.request_timeout_sec)
        except asyncio.TimeoutError as e:
            self._pending.pop(req_id, None)
            raise TimeoutError(f"Grok ACP {method} timed out") from e

    async def _read_loop(self) -> None:
        """Read stdout in chunks (not readline) — Grok emits large JSON lines
        that exceed asyncio's default 64KiB StreamReader limit."""
        assert self._proc and self._proc.stdout
        buf = b""
        try:
            while True:
                chunk = await self._proc.stdout.read(65536)
                if not chunk:
                    break
                buf += chunk
                while b"\n" in buf:
                    raw, buf = buf.split(b"\n", 1)
                    line = raw.decode("utf-8", errors="replace").strip()
                    if not line:
                        continue
                    try:
                        msg = json.loads(line)
                    except json.JSONDecodeError:
                        logger.debug("[%s] non-json stdout: %s", self.log_label, line[:200])
                        continue
                    await self._handle_message(msg)
        except asyncio.CancelledError:
            raise
        except Exception as e:
            logger.warning("[%s] read loop ended: %s", self.log_label, e)
        finally:
            for fut in list(self._pending.values()):
                if not fut.done():
                    fut.set_exception(RuntimeError("Grok ACP stdout closed"))
            self._pending.clear()

    async def _handle_message(self, msg: dict[str, Any]) -> None:
        # Notifications (no id)
        method = msg.get("method")
        if method == "session/update":
            update = (msg.get("params") or {}).get("update") or {}
            kind = update.get("sessionUpdate")
            if kind == "agent_message_chunk":
                content = update.get("content") or {}
                text = content.get("text") if isinstance(content, dict) else None
                if text:
                    self._chunks.append(str(text))
                    await self._emit_stream("message", str(text))
            elif kind == "agent_thought_chunk":
                content = update.get("content") or {}
                text = content.get("text") if isinstance(content, dict) else None
                if text:
                    logger.debug("[%s] thought: %s", self.log_label, str(text)[:160])
                    await self._emit_stream("thought", str(text))
            elif kind in {"tool_call", "tool_call_update", "agent_tool_call", "tool_call_start"}:
                title = update.get("title") or update.get("toolName") or update.get("name") or kind
                status = update.get("status") or update.get("kind") or ""
                msg = f"tool: {title} {status}".strip()
                await self._emit_stream("tool", msg, force=True)
            return

        # Permission requests from agent → auto-approve when configured
        if method in {"request_permission", "session/request_permission"}:
            req_id = msg.get("id")
            if req_id is not None and self._proc and self._proc.stdin:
                result = {"outcome": {"outcome": "selected", "optionId": "allow-always"}}
                if self.always_approve:
                    # Grok variants differ; send a permissive shape.
                    result = {"approved": True, "optionId": "allow-always"}
                resp = {"jsonrpc": "2.0", "id": req_id, "result": result}
                self._proc.stdin.write((json.dumps(resp) + "\n").encode("utf-8"))
                await self._proc.stdin.drain()
            return

        # Response to our request
        if "id" in msg and ("result" in msg or "error" in msg):
            req_id = msg["id"]
            fut = self._pending.pop(req_id, None)
            if not fut or fut.done():
                return
            if "error" in msg:
                err = msg["error"]
                fut.set_exception(
                    RuntimeError(err.get("message") if isinstance(err, dict) else str(err))
                )
            else:
                fut.set_result(msg.get("result") or {})
            return
