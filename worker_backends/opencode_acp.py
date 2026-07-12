"""OpenCode ACP backend — JSON-RPC 2.0 over stdio to `opencode acp`.

Lifecycle: initialize → authenticate (optional) → session/new → session/prompt.
Assistant text streams as session/update agent_message_chunk notifications.

This is intentionally raw JSON-RPC (no `acp` Python library); the wire shape
mirrors GrokAcpBackend because OpenCode uses the canonical ACP method names
with content-block prompts.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import time
from pathlib import Path
from typing import Any

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


class OpencodeAcpBackend:
    """Long-lived ACP client for the OpenCode CLI.

        backend = OpencodeAcpBackend(model="anthropic/claude-sonnet-4")
        await backend.start()
        text, ctx = await backend.run("Say hi", {})
        text, ctx = await backend.run("Continue", ctx)
        await backend.close()

    Sessions survive across turns via ctx["opencode_acp_session_id"].
    """

    def __init__(
        self,
        *,
        cwd: str | None = None,
        opencode_cmd: str | None = None,
        model: str | None = None,
        provider: str | None = None,
        always_approve: bool = True,
        log_label: str = "opencode-acp",
        request_timeout_sec: float = 900.0,
        acp_args: list[str] | None = None,
    ) -> None:
        self.cwd = str(Path(cwd or os.getcwd()).resolve())
        self.opencode_cmd = opencode_cmd or resolve_opencode_bin()
        self.model = model
        self.provider = provider
        self.always_approve = always_approve
        self.log_label = log_label
        self.request_timeout_sec = request_timeout_sec
        self.acp_args = list(acp_args or [])

        self._proc: asyncio.subprocess.Process | None = None
        self._reader_task: asyncio.Task[None] | None = None
        self._pending: dict[int, asyncio.Future[Any]] = {}
        self._next_id = 1
        self._started = False
        self._chunks: list[str] = []
        self._thought_chunks: list[str] = []
        self._lock = asyncio.Lock()
        self._progress_handler: Any = None
        self._stream_buf: str = ""
        self._thought_buf: str = ""
        self._last_stream_emit: float = 0.0
        self._stream_min_interval: float = 0.35
        self._stream_min_chars: int = 40

    async def start(self) -> None:
        if self._started:
            return
        cmd = [self.opencode_cmd, "acp", *self.acp_args]
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

        init = await self._request("initialize", {
            "protocolVersion": 1,
            "clientInfo": {"name": "nats-hub-opencode-acp", "version": "0.1.0"},
            "clientCapabilities": {
                "fs": {"readTextFile": True, "writeTextFile": True},
                "terminal": True,
            },
        })

        auth_methods = {
            str(m.get("id"))
            for m in (init.get("authMethods") or [])
            if isinstance(m, dict) and m.get("id")
        }
        if not auth_methods:
            return
        chosen: str | None = None
        if "env" in auth_methods and self._any_provider_env():
            chosen = "env"
        else:
            meta = init.get("_meta") or {}
            default_auth = meta.get("defaultAuthMethodId")
            if isinstance(default_auth, str) and default_auth in auth_methods:
                chosen = default_auth
            else:
                chosen = sorted(auth_methods)[0]
        try:
            await self._request(
                "authenticate",
                {"methodId": chosen, "_meta": {"headless": True}},
            )
            logger.info("[%s] authenticated via %s", self.log_label, chosen)
        except Exception as e:
            logger.debug("[%s] authenticate(%s) ignored: %s", self.log_label, chosen, e)

    @staticmethod
    def _any_provider_env() -> bool:
        return any(os.environ.get(v) for v in (
            "ANTHROPIC_API_KEY", "OPENAI_API_KEY",
            "GOOGLE_API_KEY", "GEMINI_API_KEY", "OPENCODE_API_KEY",
        ))

    async def close(self) -> None:
        """Shut down the ACP subprocess and reader task."""
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
                fut.set_exception(RuntimeError("OpenCode ACP closed"))
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

    def set_progress_handler(self, handler) -> None:
        self._progress_handler = handler

    def clear_progress_handler(self) -> None:
        self._progress_handler = None
        self._stream_buf = ""
        self._thought_buf = ""

    async def _emit_stream(self, kind: str, text: str, *, force: bool = False) -> None:
        if not self._progress_handler:
            return
        now = time.monotonic()
        if kind == "tool":
            try:
                await self._progress_handler("tool", {"text": text, "message": text})
            except Exception as e:
                logger.debug("[%s] progress handler error: %s", self.log_label, e)
            self._last_stream_emit = now
            return
        buf_attr = "_thought_buf" if kind == "thought" else "_stream_buf"
        buf = getattr(self, buf_attr)
        if text:
            buf += text
            setattr(self, buf_attr, buf)
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
        """Send a prompt; return (assistant_text, ctx). Reuses ctx session id."""
        async with self._lock:
            if not self._started:
                await self.start()
            ctx = dict(ctx or {})
            session_id = ctx.get("opencode_acp_session_id")

            if not session_id:
                session_id = await self._new_session()
                ctx["opencode_acp_session_id"] = session_id
                ctx["acp_session_id"] = session_id
            elif self.model and self.model != ctx.get("opencode_acp_model"):
                try:
                    await self._request("session/set_model", {
                        "sessionId": session_id,
                        "modelId": self.model,
                        **_self_meta(self.provider),
                    }, timeout=30.0)
                    ctx["opencode_acp_model"] = self.model
                except Exception as e:
                    logger.debug("[%s] set_model (resume) ignored: %s", self.log_label, e)

            self._chunks.clear()
            self._thought_chunks.clear()
            self._stream_buf = ""
            self._thought_buf = ""
            self._last_stream_emit = 0.0
            logger.info("[%s] prompt: %s", self.log_label, prompt[:100])

            result = await self._request("session/prompt", {
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": prompt}],
            }, timeout=self.request_timeout_sec)

            if self._stream_buf:
                await self._emit_stream("message", "", force=True)
            if self._thought_buf:
                await self._emit_stream("thought", "", force=True)

            text = "".join(self._chunks).strip()
            if not text and isinstance(result, dict):
                text = result.get("text") or result.get("result") or result.get("message") or ""
                if isinstance(text, dict):
                    text = text.get("text") or json.dumps(text)
                text = str(text).strip()
            if not text:
                stop = result.get("stopReason") if isinstance(result, dict) else None
                text = f"(no text returned; stopReason={stop})"
            return text, ctx

    async def _new_session(self) -> str:
        new = await self._request("session/new", {
            "cwd": self.cwd,
            "mcpServers": [],
            **_self_meta(self.provider),
        })
        session_id = new.get("sessionId")
        if not session_id:
            raise RuntimeError(f"session/new missing sessionId: {new}")
        logger.info("[%s] session %s", self.log_label, str(session_id)[:12])
        if self.model:
            try:
                await self._request("session/set_model", {
                    "sessionId": session_id,
                    "modelId": self.model,
                    **_self_meta(self.provider),
                }, timeout=30.0)
            except Exception as e:
                logger.debug("[%s] set_model ignored: %s", self.log_label, e)
        if self.always_approve:
            try:
                await self._request("session/set_mode", {
                    "sessionId": session_id,
                    "modeId": "always-allow",
                }, timeout=15.0)
            except Exception:
                pass
        return session_id

    async def _request(
        self,
        method: str,
        params: dict[str, Any] | None = None,
        *,
        timeout: float | None = None,
    ) -> Any:
        if not self._proc or not self._proc.stdin:
            raise RuntimeError("OpenCode ACP process not started")
        req_id = self._next_id
        self._next_id += 1
        payload = {"jsonrpc": "2.0", "id": req_id, "method": method, "params": params or {}}
        fut: asyncio.Future[Any] = asyncio.get_running_loop().create_future()
        self._pending[req_id] = fut
        self._proc.stdin.write((json.dumps(payload) + "\n").encode("utf-8"))
        await self._proc.stdin.drain()
        try:
            return await asyncio.wait_for(fut, timeout=timeout or self.request_timeout_sec)
        except asyncio.TimeoutError as e:
            self._pending.pop(req_id, None)
            raise TimeoutError(f"OpenCode ACP {method} timed out") from e

    async def _read_loop(self) -> None:
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
                    fut.set_exception(RuntimeError("OpenCode ACP stdout closed"))
            self._pending.clear()

    async def _handle_message(self, msg: dict[str, Any]) -> None:
        method = msg.get("method")
        if method == "session/update":
            update = (msg.get("params") or {}).get("update") or {}
            kind = update.get("sessionUpdate")
            if kind in ("agent_message_chunk", "agentMessageChunk"):
                content = update.get("content") or {}
                text = content.get("text") if isinstance(content, dict) else None
                if text:
                    self._chunks.append(str(text))
                    await self._emit_stream("message", str(text))
            elif kind in ("agent_thought_chunk", "agentThoughtChunk"):
                content = update.get("content") or {}
                text = content.get("text") if isinstance(content, dict) else None
                if text:
                    self._thought_chunks.append(str(text))
                    await self._emit_stream("thought", str(text))
            elif kind in ("tool_call", "tool_call_update", "agent_tool_call", "tool_call_start"):
                title = update.get("title") or update.get("toolName") or update.get("name") or kind
                status = update.get("status") or update.get("kind") or ""
                await self._emit_stream("tool", f"tool: {title} {status}".strip(), force=True)
            return

        if method in {"request_permission", "session/request_permission"}:
            req_id = msg.get("id")
            if req_id is not None and self._proc and self._proc.stdin:
                result = (
                    {"outcome": {"outcome": "selected", "optionId": "allow-always"},
                     "approved": True, "optionId": "allow-always"}
                    if self.always_approve
                    else {"outcome": {"outcome": "denied"}}
                )
                self._proc.stdin.write(
                    (json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result}) + "\n").encode("utf-8")
                )
                await self._proc.stdin.drain()
            return

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


def _self_meta(provider: str | None) -> dict[str, Any]:
    if provider:
        return {"_meta": {"opencode": {"provider": provider}}}
    return {}


__all__ = ["OpencodeAcpBackend", "resolve_opencode_bin"]