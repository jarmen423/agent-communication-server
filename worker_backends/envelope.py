"""Envelope + prompt helpers for the Python worker runtime (``worker_runtime``
re-exports them; split out to keep the runtime under the size guideline)."""

from __future__ import annotations

import asyncio
import json
import signal
import uuid
from datetime import datetime, timezone

# Payload keys a one-shot prompt may arrive under (bridges send "message").
PROMPT_KEYS = ("prompt", "text", "command", "message")


def make_envelope(
    from_id: str,
    to_id: str | None,
    channel: str,
    kind: str,
    payload: dict,
    reply_to: str | None = None,
) -> bytes:
    meta = {
        "id": str(uuid.uuid4()),
        "from": from_id,
        "channel": channel,
        "kind": kind,
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }
    if to_id:
        meta["to"] = to_id
    if reply_to:
        meta["reply_to"] = reply_to
    return json.dumps({"meta": meta, "payload": payload}).encode()


def extract_prompt(payload: dict) -> str | None:
    for key in PROMPT_KEYS:
        value = payload.get(key)
        if isinstance(value, str) and value:
            return value
    return None


def install_stop_signals(task: asyncio.Task) -> None:
    """SIGTERM/SIGHUP → cancel ``task`` so run_worker closes NATS cleanly.

    Skipped for any signal that already has a handler, so an entrypoint that
    installs its own (e.g. forwarding to CLI process groups) keeps it.
    """
    loop = asyncio.get_running_loop()
    for name in ("SIGTERM", "SIGHUP"):
        sig = getattr(signal, name, None)
        if sig is None or signal.getsignal(sig) is not signal.SIG_DFL:
            continue
        try:
            loop.add_signal_handler(sig, task.cancel)
        except (NotImplementedError, RuntimeError, ValueError):
            pass  # non-main thread or unsupported platform


def is_task_result(payload: dict) -> bool:
    """Results carry task_id + status; never answer one (avoids DM ping-pong)."""
    return "task_id" in payload and "status" in payload
