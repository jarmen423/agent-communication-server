"""Child-process mechanics for worker_supervisor.py.

- every child runs in its own process group with stdout+stderr appended to
  ``<log_dir>/<identity>.log`` (PYTHONUNBUFFERED=1 so lines land promptly);
- readiness is detected from the log (the runtime prints
  ``subscribed to channel.inbox.<id>`` / ``] ready``) or from a heartbeat,
  whichever comes first — the first heartbeat only arrives after 30s;
- crashed children are restarted with exponential backoff, bounded by
  ``RestartPolicy`` (N restarts per window, then give up);
- stop/shutdown SIGTERM the child's process group, then SIGKILL.
"""

from __future__ import annotations

import asyncio
import os
import re
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from worker_backends import model_catalog
from worker_backends.proc import kill_group

READY_MARKERS = ("subscribed to channel.inbox.", "] ready")
_SAFE_NAME = re.compile(r"[^A-Za-z0-9._-]+")


def log_path_for(log_dir: Path, identity: str) -> Path:
    name = _SAFE_NAME.sub("_", identity).strip("._") or "worker"
    return log_dir / f"{name}.log"


@dataclass
class RestartPolicy:
    max_restarts: int = 5      # within window_sec; then the child is abandoned
    window_sec: float = 300.0
    base_backoff: float = 1.0
    max_backoff: float = 30.0

    def next_delay(self, restart_times: list[float], now: float | None = None) -> float | None:
        """Backoff before the next restart, or None when the budget is spent.

        Prunes ``restart_times`` to the current window in place.
        """
        now = time.monotonic() if now is None else now
        restart_times[:] = [t for t in restart_times if now - t < self.window_sec]
        n = len(restart_times)
        if n >= self.max_restarts:
            return None
        return min(self.base_backoff * (2 ** n), self.max_backoff)


@dataclass
class Child:
    identity: str
    provider: str
    argv: list[str]
    proc: asyncio.subprocess.Process
    log_path: Path
    model: str | None = None
    started_at: float = field(default_factory=time.time)
    ready: bool = False
    stopping: bool = False
    restarts: int = 0
    restart_times: list[float] = field(default_factory=list)
    log_offset: int = 0  # log size when this incarnation started

    @property
    def alive(self) -> bool:
        return self.proc.returncode is None


async def spawn_child(
    argv: list[str], *, cwd: str, log_path: Path, env: dict[str, str] | None = None
) -> tuple[asyncio.subprocess.Process, int]:
    """Start ``argv`` in a new process group, output appended to ``log_path``.

    Returns (process, log offset before this incarnation's output).
    """
    log_path.parent.mkdir(parents=True, exist_ok=True)
    child_env = dict(os.environ if env is None else env)
    child_env.setdefault("PYTHONUNBUFFERED", "1")
    with open(log_path, "ab") as log:
        stamp = time.strftime("%Y-%m-%dT%H:%M:%S")
        log.write(f"\n==== {stamp} spawn: {' '.join(argv)}\n".encode())
        log.flush()
        offset = log.tell()
        proc = await asyncio.create_subprocess_exec(
            *argv,
            cwd=cwd,
            stdin=asyncio.subprocess.DEVNULL,
            stdout=log,
            stderr=asyncio.subprocess.STDOUT,
            env=child_env,
            start_new_session=True,
        )
    return proc, offset


async def wait_log_marker(
    log_path: Path,
    offset: int,
    proc: asyncio.subprocess.Process,
    markers: tuple[str, ...] = READY_MARKERS,
    poll: float = 0.1,
) -> bool:
    """Poll the log from ``offset`` until a marker appears (True) or the
    process exits (False). Callers bound this with ``asyncio.wait_for``."""
    seen = b""
    encoded = [m.encode() for m in markers]
    while True:
        try:
            with open(log_path, "rb") as f:
                f.seek(offset)
                data = f.read()
        except OSError:
            data = b""
        if data:
            offset += len(data)
            seen = (seen + data)[-8192:]
            if any(m in seen for m in encoded):
                return True
        if proc.returncode is not None:
            return False
        await asyncio.sleep(poll)


async def stop_child(child: Child, grace: float = 5.0) -> None:
    child.stopping = True
    await kill_group(child.proc, grace=grace)


def tail_file(path: Path, max_bytes: int = 2000) -> str:
    try:
        size = path.stat().st_size
        with open(path, "rb") as f:
            f.seek(max(0, size - max_bytes))
            return f.read().decode("utf-8", errors="replace")
    except OSError:
        return ""


async def models_request(req: dict[str, Any]) -> dict[str, Any]:
    """Body of a `hub.worker.models` request-reply ({provider, refresh?})."""
    provider = (req.get("provider") or "").strip().lower()
    if not provider:
        return {"ok": False, "error": "provider required", "models": []}
    try:
        refresh = bool(req.get("refresh"))
        if refresh:
            model_catalog.clear_cache(provider)
        return await model_catalog.list_models(provider, use_cache=not refresh)
    except Exception as e:
        return {"ok": False, "provider": provider, "error": str(e), "models": []}


def providers_request(spawnable: list[str]) -> dict[str, Any]:
    """Body of a `hub.worker.providers` reply, annotated with spawnable ids."""
    try:
        result = model_catalog.list_provider_catalog()
        for p in result.get("providers") or []:
            p["spawnable"] = p.get("id") in spawnable
        result["spawnable"] = sorted(spawnable)
        return result
    except Exception as e:
        return {"ok": False, "error": str(e), "providers": []}
