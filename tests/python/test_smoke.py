"""Smoke tests for the Python side. Unit tests always run; the live test runs
only under scripts/dev/with_stack.sh (NATS_URL + hub-server available)."""
from __future__ import annotations

import asyncio
import json
import os
import subprocess
import sys
import uuid

import pytest

from nats_connect import build_tls_context
from worker_runtime import make_envelope

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


def test_make_envelope_shape():
    raw = make_envelope("alice", "bob", "tasks", "message", {"prompt": "hi"}, reply_to="abc")
    env = json.loads(raw)
    meta = env["meta"]
    assert meta["from"] == "alice"
    assert meta["to"] == "bob"
    assert meta["channel"] == "tasks"
    assert meta["kind"] == "message"
    assert meta["reply_to"] == "abc"
    assert env["payload"] == {"prompt": "hi"}


def test_make_envelope_broadcast_omits_to():
    env = json.loads(make_envelope("alice", None, "news", "status", {}))
    assert "to" not in env["meta"]
    assert "reply_to" not in env["meta"]


def test_tls_context_none_for_plain_url():
    assert (
        build_tls_context(ca_file=None, cert_file=None, key_file=None, tls_insecure=False, url="nats://x:4222")
        is None
    )


def test_tls_insecure_requires_opt_in(monkeypatch):
    monkeypatch.delenv("NATS_ALLOW_INSECURE", raising=False)
    with pytest.raises(ValueError):
        build_tls_context(ca_file=None, cert_file=None, key_file=None, tls_insecure=True, url="tls://x:4222")


live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh (nats-server + hub-server)",
)


@live
def test_echo_worker_round_trip():
    """Delegate to echo_worker.py through the real router; expect a terminal
    `message` on the task channel (reply contract, refocus.md §6)."""
    import nats

    async def run() -> dict:
        url = os.environ["NATS_URL"]
        identity = f"echo-{uuid.uuid4().hex[:6]}"
        worker = subprocess.Popen(
            [sys.executable, "echo_worker.py", "--identity", identity, "--nats-url", url],
            cwd=REPO_ROOT,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        nc = await nats.connect(url)
        try:
            await asyncio.sleep(1.5)  # let the worker subscribe
            task_channel = f"task.{uuid.uuid4().hex[:8]}"
            sub = await nc.subscribe(f"channel.{task_channel}")
            await nc.flush()
            await nc.publish(
                f"hub.send.{task_channel}",
                make_envelope("pytest", identity, task_channel, "message",
                              {"prompt": "abc", "task_channel": task_channel}),
            )
            deadline = asyncio.get_running_loop().time() + 15
            while True:
                remaining = deadline - asyncio.get_running_loop().time()
                msg = await sub.next_msg(timeout=max(remaining, 0.1))
                env = json.loads(msg.data)
                if env["meta"]["kind"] == "message":
                    return env
        finally:
            await nc.close()
            worker.terminate()
            worker.wait(timeout=5)

    env = asyncio.run(run())
    assert env["payload"].get("result") == "echo: cba"
