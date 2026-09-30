"""Shared helpers for tests/python/test_worker_*.py."""
from __future__ import annotations

import os
import time
from pathlib import Path
from typing import Any

FIXTURES = Path(__file__).resolve().parent
BIN = FIXTURES / "bin"
MISBEHAVE = FIXTURES / "misbehave.py"
FAKE_ACP = FIXTURES / "fake_acp_agent.py"
FAKE_WORKER = FIXTURES / "fake_worker.py"


def pid_alive(pid: int) -> bool:
    """True if ``pid`` is a live (non-zombie) process."""
    if Path("/proc/self/stat").exists():  # Linux: zombies count as dead
        try:
            state = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]
        except (OSError, IndexError):
            return False
        return state not in ("Z", "X")
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def wait_dead(pids: list[int], timeout: float = 5.0) -> list[int]:
    """Wait until every pid is gone; return the ones still alive."""
    deadline = time.monotonic() + timeout
    alive = list(pids)
    while alive and time.monotonic() < deadline:
        alive = [p for p in alive if pid_alive(p)]
        if alive:
            time.sleep(0.05)
    return alive


def wait_for_file(path: Path, timeout: float = 5.0) -> str:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists() and path.read_text().strip():
            return path.read_text().strip()
        time.sleep(0.05)
    raise AssertionError(f"{path} was never written")


class ProgressRecorder:
    """Stands in for worker_events' progress handler."""

    def __init__(self) -> None:
        self.events: list[tuple[str, dict[str, Any]]] = []

    async def __call__(self, kind: str, data: dict[str, Any]) -> None:
        self.events.append((kind, data))

    def kinds(self) -> list[str]:
        return [k for k, _ in self.events]

    def texts(self, kind: str) -> list[str]:
        return [d.get("text") or d.get("message") or "" for k, d in self.events if k == kind]
