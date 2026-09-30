"""Shared JSON-RPC 2.0 over stdio plumbing for ACP agents (grok, opencode).

Lifecycle: spawn → initialize → authenticate → session/new → session/prompt.
Assistant text streams as ``session/update`` notifications.

Hardening:
  - stderr is drained continuously into a bounded tail buffer (no pipe
    deadlock) and quoted in errors when the agent dies;
  - we advertise NO ``fs``/``terminal`` client capabilities (we do not
    implement them) and answer any unexpected agent→client request with
    JSON-RPC "method not found" instead of leaving the agent hanging;
  - ``session/request_permission`` is answered by picking an option from the
    offered ``options`` by *kind*, per ``permission_policy``;
  - when stdout hits EOF the backend is marked dead, pending requests fail
    with the stderr tail, and the next ``run()`` restarts the agent process
    (sessions from the dead process are not reused).
"""

from __future__ import annotations

import asyncio
import json
import logging
import time
from pathlib import Path
from typing import Any

from worker_backends.proc import StderrTail, drain_stream, kill_group, read_lines

logger = logging.getLogger(__name__)

PERMISSION_POLICIES = ("allow_once", "allow_always", "reject")
_POLICY_PREFERENCE = {
    "allow_always": ("allow_always", "allow_once"),
    "allow_once": ("allow_once", "allow_always"),
    "reject": ("reject_once", "reject_always"),
}
CLIENT_CAPABILITIES: dict[str, Any] = {
    "fs": {"readTextFile": False, "writeTextFile": False},
    "terminal": False,
}
_MESSAGE_CHUNKS = ("agent_message_chunk", "agentMessageChunk")
_THOUGHT_CHUNKS = ("agent_thought_chunk", "agentThoughtChunk")
_TOOL_UPDATES = ("tool_call", "tool_call_update", "agent_tool_call", "tool_call_start")


def choose_permission_option(options: Any, policy: str) -> str | None:
    """Pick an ``optionId`` from ACP permission ``options`` by kind."""
    if not isinstance(options, list):
        return None
    for kind in _POLICY_PREFERENCE.get(policy, ()):
        for opt in options:
            if isinstance(opt, dict) and opt.get("kind") == kind and opt.get("optionId"):
                return str(opt["optionId"])
    return None


def permission_result(params: dict[str, Any], policy: str) -> dict[str, Any]:
    chosen = choose_permission_option((params or {}).get("options"), policy)
    if chosen:
        return {"outcome": {"outcome": "selected", "optionId": chosen}}
    return {"outcome": {"outcome": "cancelled"}}


class AcpStdioBackend:
    """Base class: subclasses supply the command, auth and session setup."""

    label = "ACP agent"
    session_ctx_key = "acp_session_id"

    def __init__(
        self,
        *,
        cwd: str | None,
        log_label: str,
        request_timeout_sec: float = 900.0,
        permission_policy: str = "allow_once",
        stderr_tail_bytes: int = 16 * 1024,
    ) -> None:
        if permission_policy not in PERMISSION_POLICIES:
            raise ValueError(f"permission_policy must be one of {PERMISSION_POLICIES}")
        self.cwd = str(Path(cwd or ".").resolve())
        self.log_label = log_label
        self.request_timeout_sec = request_timeout_sec
        self.permission_policy = permission_policy
        self.stderr = StderrTail(stderr_tail_bytes)
        self.generation = 0
        self._proc: asyncio.subprocess.Process | None = None
        self._tasks: list[asyncio.Task[None]] = []
        self._pending: dict[int, asyncio.Future[Any]] = {}
        self._next_id = 1
        self._alive = False
        self._chunks: list[str] = []
        self._lock = asyncio.Lock()
        self._progress_handler: Any = None
        self._stream_buf = ""
        self._thought_buf = ""
        self._last_stream_emit = 0.0
        self._stream_min_interval = 0.35  # throttle hub events
        self._stream_min_chars = 40

    # ── subclass hooks ─────────────────────────────────────────────────

    def _command(self) -> list[str]:
        raise NotImplementedError

    async def _authenticate(self, init: dict[str, Any]) -> None:
        """Pick and run an auth method from the initialize result."""

    async def _new_session(self) -> str:
        new = await self._request("session/new", {"cwd": self.cwd, "mcpServers": []})
        session_id = (new or {}).get("sessionId")
        if not session_id:
            raise RuntimeError(f"session/new missing sessionId: {new}")
        return str(session_id)

    async def _on_resume(self, session_id: str, ctx: dict[str, Any]) -> None:
        """Called before a turn on an existing session (e.g. model switch)."""

    # ── lifecycle ──────────────────────────────────────────────────────

    @property
    def alive(self) -> bool:
        return self._alive and self._proc is not None and self._proc.returncode is None

    async def start(self) -> None:
        if self.alive:
            return
        await self.close()
        cmd = self._command()
        logger.info("[%s] spawning %s (cwd=%s)", self.log_label, " ".join(cmd), self.cwd)
        self._proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=self.cwd,
            start_new_session=True,
        )
        self.generation += 1
        self._alive = True
        self._tasks = [
            asyncio.create_task(self._read_loop(self._proc)),
            asyncio.create_task(drain_stream(self._proc.stderr, self.stderr.feed)),
        ]
        try:
            init = await self._request("initialize", {
                "protocolVersion": 1,
                "clientInfo": {"name": "nats-hub", "version": "0.1.0"},
                "clientCapabilities": CLIENT_CAPABILITIES,
            }, timeout=60.0)
            await self._authenticate(init if isinstance(init, dict) else {})
        except BaseException:
            await self.close()
            raise

    async def close(self) -> None:
        self._alive = False
        proc, self._proc = self._proc, None  # detach first: reader skips its EOF path
        for task in self._tasks:
            task.cancel()
        for task in self._tasks:
            try:
                await task
            except (asyncio.CancelledError, Exception):
                pass
        self._tasks = []
        self._fail_pending(f"{self.label} closed")
        if proc is not None:
            try:
                if proc.stdin:
                    proc.stdin.close()
            except Exception:
                pass
            await kill_group(proc)

    # ── turns ──────────────────────────────────────────────────────────

    def set_progress_handler(self, handler) -> None:
        """Optional async callback: handler(kind: str, data: dict)."""
        self._progress_handler = handler

    def clear_progress_handler(self) -> None:
        self._progress_handler = None
        self._stream_buf = ""
        self._thought_buf = ""

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        async with self._lock:
            if not self.alive:
                if self.generation:
                    logger.warning("[%s] agent process is gone; restarting", self.log_label)
                await self.start()
            ctx = dict(ctx or {})
            gen_key = f"{self.session_ctx_key}_generation"
            session_id = ctx.get(self.session_ctx_key) if ctx.get(gen_key) == self.generation else None
            if session_id:
                await self._on_resume(session_id, ctx)
            else:
                session_id = await self._new_session()
                logger.info("[%s] session %s", self.log_label, session_id[:12])
                ctx.update({self.session_ctx_key: session_id, "acp_session_id": session_id,
                            gen_key: self.generation})

            self._chunks.clear()
            self._stream_buf = self._thought_buf = ""
            self._last_stream_emit = 0.0
            result = await self._request("session/prompt", {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": prompt}],
            }, timeout=self.request_timeout_sec)

            # Flush any throttled tail so visualizers see the final partials.
            if self._stream_buf:
                await self._emit_stream("message", "", force=True)
            if self._thought_buf:
                await self._emit_stream("thought", "", force=True)
            return self._final_text(result), ctx

    def _final_text(self, result: Any) -> str:
        text = "".join(self._chunks).strip()
        if not text and isinstance(result, dict):
            raw = result.get("text") or result.get("result") or result.get("message") or ""
            if isinstance(raw, dict):
                raw = raw.get("text") or json.dumps(raw)
            text = str(raw).strip()
        if not text:
            stop = result.get("stopReason") if isinstance(result, dict) else None
            text = f"(no text returned; stopReason={stop})"
        return text

    async def _emit_stream(self, kind: str, text: str, *, force: bool = False) -> None:
        """Throttle streaming hub publishes so we don't flood NATS."""
        if not self._progress_handler:
            return
        now = time.monotonic()
        try:
            if kind == "tool":
                await self._progress_handler("tool", {"text": text, "message": text})
                self._last_stream_emit = now
                return
            attr = "_thought_buf" if kind == "thought" else "_stream_buf"
            buf = getattr(self, attr) + (text or "")
            setattr(self, attr, buf)
            if not buf:
                return
            due = (now - self._last_stream_emit) >= self._stream_min_interval
            if force or due or len(buf) >= self._stream_min_chars:
                await self._progress_handler(
                    "thought" if kind == "thought" else "message",
                    {"text": buf[-800:], "delta": text, "full_len": len(buf)},
                )
                self._last_stream_emit = now
        except Exception as e:
            logger.debug("[%s] progress handler error: %s", self.log_label, e)

    # ── JSON-RPC plumbing ──────────────────────────────────────────────

    def _dead_detail(self) -> str:
        code = self._proc.returncode if self._proc else None
        tail = self.stderr.text(1500)
        return f"{self.label} exited (code={code})" + (f"; stderr: {tail}" if tail else "")

    def _fail_pending(self, reason: str) -> None:
        for fut in self._pending.values():
            if not fut.done():
                fut.set_exception(RuntimeError(reason))
        self._pending.clear()

    async def _send(self, obj: dict[str, Any]) -> None:
        if not self.alive or not self._proc or not self._proc.stdin:
            raise RuntimeError(self._dead_detail() if self.generation else f"{self.label} not started")
        self._proc.stdin.write((json.dumps(obj) + "\n").encode("utf-8"))
        await self._proc.stdin.drain()

    async def _request(self, method: str, params: dict[str, Any] | None = None,
                       *, timeout: float | None = None) -> Any:
        req_id = self._next_id
        self._next_id += 1
        fut: asyncio.Future[Any] = asyncio.get_running_loop().create_future()
        self._pending[req_id] = fut
        try:
            await self._send({"jsonrpc": "2.0", "id": req_id, "method": method, "params": params or {}})
            return await asyncio.wait_for(fut, timeout=timeout or self.request_timeout_sec)
        except asyncio.TimeoutError as e:
            raise TimeoutError(f"{self.label} {method} timed out") from e
        except (BrokenPipeError, ConnectionResetError) as e:
            self._alive = False
            raise RuntimeError(self._dead_detail()) from e
        finally:
            self._pending.pop(req_id, None)

    async def _read_loop(self, proc: asyncio.subprocess.Process) -> None:
        async def on_line(line: str) -> None:
            line = line.strip()
            if not line:
                return
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                logger.debug("[%s] non-json stdout: %s", self.log_label, line[:200])
                return
            if isinstance(msg, dict):
                await self._handle_message(msg)

        try:
            await read_lines(proc.stdout, on_line)
        except asyncio.CancelledError:
            raise
        except Exception as e:
            logger.warning("[%s] read loop ended: %s", self.log_label, e)
        finally:
            if self._proc is proc:
                self._alive = False
                try:
                    await asyncio.wait_for(proc.wait(), timeout=2.0)
                except (asyncio.TimeoutError, asyncio.CancelledError):
                    pass
                self._fail_pending(self._dead_detail())
                logger.warning("[%s] %s", self.log_label, self._dead_detail())

    async def _handle_message(self, msg: dict[str, Any]) -> None:
        method = msg.get("method")
        if method == "session/update":
            await self._on_update((msg.get("params") or {}).get("update") or {})
            return
        if method in ("request_permission", "session/request_permission"):
            if msg.get("id") is not None:
                result = permission_result(msg.get("params") or {}, self.permission_policy)
                await self._send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
            return
        if method and msg.get("id") is not None:
            # fs/*, terminal/* or anything else we don't implement.
            await self._send({"jsonrpc": "2.0", "id": msg["id"],
                              "error": {"code": -32601, "message": f"method not supported: {method}"}})
            return
        if "id" in msg and ("result" in msg or "error" in msg):
            fut = self._pending.get(msg["id"])
            if not fut or fut.done():
                return
            if "error" in msg:
                err = msg["error"]
                fut.set_exception(RuntimeError(err.get("message") if isinstance(err, dict) else str(err)))
            else:
                fut.set_result(msg.get("result") or {})

    async def _on_update(self, update: dict[str, Any]) -> None:
        kind = update.get("sessionUpdate")
        content = update.get("content") or {}
        text = content.get("text") if isinstance(content, dict) else None
        if kind in _MESSAGE_CHUNKS and text:
            self._chunks.append(str(text))
            await self._emit_stream("message", str(text))
        elif kind in _THOUGHT_CHUNKS and text:
            await self._emit_stream("thought", str(text))
        elif kind in _TOOL_UPDATES:
            title = update.get("title") or update.get("toolName") or update.get("name") or kind
            status = update.get("status") or update.get("kind") or ""
            await self._emit_stream("tool", f"tool: {title} {status}".strip(), force=True)
