"""Helpers for `live` worker tests (need scripts/dev/with_stack.sh)."""
from __future__ import annotations

import asyncio
import json
import os
import subprocess
import time
import uuid
from pathlib import Path
from typing import Any

import pytest

from fixtures.helpers import BIN, FAKE_ACP
from worker_runtime import make_envelope

REPO_ROOT = Path(__file__).resolve().parents[3]

live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh (nats-server + hub-server)",
)


def uniq(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex[:6]}"


def fake_env(**extra: str) -> dict[str, str]:
    """Env where every worker type finds a fake CLI/agent instead of a real one."""
    env = dict(os.environ)
    env.update({
        "PATH": f"{BIN}{os.pathsep}{env.get('PATH', '')}",
        "GROK_BIN": str(FAKE_ACP),
        "HERMES_BIN": str(FAKE_ACP),
        "OPENCODE_BIN": str(FAKE_ACP),
        "PYTHONUNBUFFERED": "1",
    })
    env.update(extra)
    return env


async def start_worker(nc, identity: str, argv: list[str], env: dict[str, str],
                       log: Path, within: float = 30.0) -> subprocess.Popen:
    """Spawn a worker and wait for its first `hub.register`."""
    reg = await nc.subscribe("hub.register.*")
    await nc.flush()
    with open(log, "ab") as out:
        proc = subprocess.Popen(argv, cwd=REPO_ROOT, env=env, stdout=out,
                                stderr=subprocess.STDOUT, start_new_session=True)
    deadline = time.monotonic() + within
    try:
        while True:
            if proc.poll() is not None:
                raise AssertionError(f"{identity} exited ({proc.returncode}):\n{log.read_text()}")
            try:
                msg = await reg.next_msg(timeout=0.5)
            except Exception:  # noqa: BLE001 - nats TimeoutError
                if time.monotonic() > deadline:
                    raise AssertionError(f"{identity} never registered:\n{log.read_text()}")
                continue
            if json.loads(msg.data)["payload"].get("identity") == identity:
                return proc
    finally:
        await reg.unsubscribe()


def stop_worker(proc: subprocess.Popen) -> None:
    if proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


async def send_task(nc, sender: str, worker: str, prompt: str) -> tuple[str, Any]:
    """DM a task with a task channel (reply contract); returns (id, channel sub)."""
    channel = uniq("task.py")
    sub = await nc.subscribe(f"channel.{channel}")
    await nc.flush()
    env = make_envelope(sender, worker, channel, "message",
                        {"prompt": prompt, "task_channel": channel})
    await nc.publish(f"hub.pub.{sender}.{channel}", env)
    await nc.flush()
    return json.loads(env)["meta"]["id"], sub


async def send_cancel(nc, sender: str, worker: str, task_id: str) -> None:
    channel = f"inbox.{worker}"
    env = make_envelope(sender, worker, channel, "control",
                        {"action": "cancel", "task_id": task_id})
    await nc.publish(f"hub.pub.{sender}.{channel}", env)
    await nc.flush()


async def wait_result(sub, task_id: str, within: float = 30.0) -> dict:
    """First `message` correlated to ``task_id`` (rule 5); returns the envelope."""
    deadline = time.monotonic() + within
    while True:
        msg = await sub.next_msg(timeout=max(deadline - time.monotonic(), 0.1))
        env = json.loads(msg.data)
        meta, payload = env["meta"], env.get("payload") or {}
        if meta.get("kind") == "message" and (
                meta.get("reply_to") == task_id or payload.get("task_id") == task_id):
            return env


def run(coro):
    return asyncio.run(coro)
