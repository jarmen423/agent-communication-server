"""Local state: which emails were handled, wake history (budget) and
per-thread agent turns (loop guard). One JSON file, written atomically.

Resend's ``read`` flag is not used for this: reading a thread through the API
can mark it read, and humans read mail in the dashboard too.
"""
from __future__ import annotations

import json
import os
import time
from pathlib import Path

MAX_PROCESSED = 20_000
DAY = 86_400.0


class State:
    def __init__(self, path: str | os.PathLike, clock=time.time):
        self.path = Path(os.path.expanduser(str(path)))
        self.clock = clock
        self.data: dict = {"processed": {}, "thread_counts": {}, "wakes": {},
                           "agent_turns": {}, "baselined": []}
        if self.path.exists():
            loaded = json.loads(self.path.read_text())
            for k, v in loaded.items():
                self.data[k] = v

    # ── persistence ────────────────────────────────────────────────────
    def save(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        processed = self.data["processed"]
        if len(processed) > MAX_PROCESSED:  # dicts keep insertion order
            for k in list(processed)[: len(processed) - MAX_PROCESSED]:
                del processed[k]
        tmp = self.path.with_suffix(self.path.suffix + ".tmp")
        tmp.write_text(json.dumps(self.data, indent=1, sort_keys=True))
        os.replace(tmp, self.path)

    # ── emails ─────────────────────────────────────────────────────────
    def is_processed(self, email_id: str) -> bool:
        return email_id in self.data["processed"]

    def mark(self, email_id: str, action: str, reason: str, **extra) -> None:
        self.data["processed"][email_id] = {"at": self.clock(), "action": action,
                                            "reason": reason, **extra}

    def update(self, email_id: str, **extra) -> None:
        self.data["processed"].setdefault(email_id, {}).update(extra)

    def thread_count(self, inbox_id: str, thread_id: str) -> int | None:
        return self.data["thread_counts"].get(f"{inbox_id}/{thread_id}")

    def set_thread_count(self, inbox_id: str, thread_id: str, n: int) -> None:
        self.data["thread_counts"][f"{inbox_id}/{thread_id}"] = n

    def baselined(self, inbox_id: str) -> bool:
        return inbox_id in self.data["baselined"]

    def set_baselined(self, inbox_id: str) -> None:
        self.data["baselined"].append(inbox_id)

    # ── budget and loop guard ──────────────────────────────────────────
    def record_wake(self, agent: str) -> None:
        now = self.clock()
        wakes = [t for t in self.data["wakes"].get(agent, []) if now - t < DAY]
        wakes.append(now)
        self.data["wakes"][agent] = wakes

    def wakes_within(self, agent: str, seconds: float) -> int:
        now = self.clock()
        return sum(1 for t in self.data["wakes"].get(agent, []) if now - t < seconds)

    def agent_turns(self, thread_id: str) -> int:
        return self.data["agent_turns"].get(thread_id, 0)

    def add_agent_turn(self, thread_id: str) -> None:
        self.data["agent_turns"][thread_id] = self.agent_turns(thread_id) + 1
