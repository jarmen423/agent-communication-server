"""Subprocess plumbing shared by worker backends (asyncio only).

- Children run in their own process group (``start_new_session=True``) so a
  timeout or shutdown can take down the whole tree, not just the direct child.
- stdout is read in chunks and split on newlines (no 64 KiB readline limit),
  so streaming CLIs (NDJSON/JSONL) can emit progress while they run.
- stderr is always drained concurrently into a bounded tail buffer, so a
  chatty child can never deadlock on a full pipe.
"""

from __future__ import annotations

import asyncio
import collections
import os
import signal
from dataclasses import dataclass
from typing import Awaitable, Callable

LineHandler = Callable[[str], Awaitable[None]]

DEFAULT_STDERR_TAIL_BYTES = 16 * 1024
KILL_GRACE_SEC = 3.0


class StderrTail:
    """Bounded buffer holding the most recent stderr output."""

    def __init__(self, max_bytes: int = DEFAULT_STDERR_TAIL_BYTES) -> None:
        self.max_bytes = max_bytes
        self._chunks: collections.deque[bytes] = collections.deque()
        self._size = 0

    def feed(self, data: bytes) -> None:
        self._chunks.append(data)
        self._size += len(data)
        while self._size > self.max_bytes and len(self._chunks) > 1:
            self._size -= len(self._chunks.popleft())

    def text(self, limit: int | None = None) -> str:
        raw = b"".join(self._chunks)[-self.max_bytes :]
        out = raw.decode("utf-8", errors="replace").strip()
        if limit is not None and len(out) > limit:
            out = "…" + out[-limit:]
        return out


async def drain_stream(
    stream: asyncio.StreamReader | None, sink: Callable[[bytes], None]
) -> None:
    """Read a stream to EOF, handing every chunk to ``sink``."""
    if stream is None:
        return
    while True:
        chunk = await stream.read(65536)
        if not chunk:
            return
        sink(chunk)


async def read_lines(stream: asyncio.StreamReader | None, on_line: LineHandler) -> None:
    """Read a stream to EOF and call ``on_line`` for each decoded line."""
    if stream is None:
        return
    buf = b""
    while True:
        chunk = await stream.read(65536)
        if not chunk:
            break
        buf += chunk
        while b"\n" in buf:
            raw, buf = buf.split(b"\n", 1)
            await on_line(raw.decode("utf-8", errors="replace").rstrip("\r"))
    if buf:
        await on_line(buf.decode("utf-8", errors="replace").rstrip("\r"))


def signal_group(proc: asyncio.subprocess.Process, sig: int) -> None:
    """Send ``sig`` to the child's process group (falls back to the child)."""
    if proc.returncode is not None:
        return
    try:
        os.killpg(proc.pid, sig)
    except (ProcessLookupError, PermissionError):
        try:
            proc.send_signal(sig)
        except ProcessLookupError:
            pass


async def kill_group(proc: asyncio.subprocess.Process, grace: float = KILL_GRACE_SEC) -> None:
    """SIGTERM the process group, wait ``grace`` seconds, then SIGKILL; always reap.

    The group is SIGKILLed even when the leader exits promptly, so grandchildren
    that ignored SIGTERM do not outlive it.
    """
    signal_group(proc, signal.SIGTERM)
    try:
        await asyncio.wait_for(proc.wait(), timeout=grace)
    except asyncio.TimeoutError:
        pass
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass
    await proc.wait()


@dataclass
class ProcResult:
    returncode: int
    stdout: str
    stderr_tail: str


class ProcTimeout(RuntimeError):
    def __init__(self, timeout: float, stderr_tail: str) -> None:
        self.timeout = timeout
        self.stderr_tail = stderr_tail
        super().__init__(f"timed out after {timeout:g}s")


async def run_streaming(
    cmd: list[str],
    *,
    cwd: str | None = None,
    timeout: float | None = None,
    on_line: LineHandler | None = None,
    env: dict[str, str] | None = None,
    stderr_tail_bytes: int = DEFAULT_STDERR_TAIL_BYTES,
) -> ProcResult:
    """Run ``cmd`` to completion in its own process group.

    stdout lines are passed to ``on_line`` as they arrive and also collected.
    On timeout (or cancellation) the whole group is killed and reaped before
    raising, so no orphan survives the call.
    """
    proc = await asyncio.create_subprocess_exec(
        *cmd,
        stdin=asyncio.subprocess.DEVNULL,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        cwd=cwd,
        env=env,
        start_new_session=True,
    )
    lines: list[str] = []
    tail = StderrTail(stderr_tail_bytes)

    async def _line(line: str) -> None:
        lines.append(line)
        if on_line is not None:
            await on_line(line)

    async def _collect() -> None:
        await asyncio.gather(read_lines(proc.stdout, _line), drain_stream(proc.stderr, tail.feed))
        await proc.wait()

    try:
        await asyncio.wait_for(_collect(), timeout=timeout)
    except asyncio.TimeoutError:
        await kill_group(proc)
        raise ProcTimeout(timeout or 0.0, tail.text(2000)) from None
    except BaseException:
        await asyncio.shield(kill_group(proc))
        raise
    return ProcResult(proc.returncode or 0, "\n".join(lines), tail.text())
