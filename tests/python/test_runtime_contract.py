"""Reply contract (refocus.md §6) for the Python worker runtime.

Unit tests always run. Tests marked `live` need scripts/dev/with_stack.sh
(nats-server + hub-server; NATS_HUB_TEST_STACK + NATS_URL set).
"""
from __future__ import annotations

import asyncio
import json
import os
import subprocess
import sys
import time
import uuid

import pytest

from worker_events import execute_with_events, result_payload, run_to_result
from worker_runtime import extract_prompt, is_task_result, make_envelope

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh (nats-server + hub-server)",
)


class _Echo:
    async def run(self, prompt, ctx):
        return f"echo: {prompt}", ctx


class _Boom:
    async def run(self, prompt, ctx):
        raise RuntimeError("kaboom")


def _record():
    sent: list[tuple[str, str, dict, str | None]] = []

    async def publish(channel, kind, payload, reply_to=None):
        sent.append((channel, kind, payload, reply_to))

    async def publish_event(channel, event_type, data, reply_to=None):
        await publish(channel, "event", {"event_type": event_type, "data": data}, reply_to=reply_to)

    return sent, publish, publish_event


# ── unit ────────────────────────────────────────────────────────────


def test_result_payload_shape():
    assert result_payload("t1", result="ok") == {
        "status": "done", "task_id": "t1", "result": "ok", "error": None,
    }
    assert result_payload("t1", error="bad") == {
        "status": "error", "task_id": "t1", "result": None, "error": "bad",
    }


@pytest.mark.parametrize("backend,status", [(_Echo(), "done"), (_Boom(), "error")])
def test_execute_with_events_correlates_everything(backend, status):
    sent, publish, publish_event = _record()
    asyncio.run(execute_with_events(
        publish=publish, publish_event_fn=publish_event, backend=backend,
        channel="task.x", prompt="hi", ctx={}, task_id="TID",
        working_status="working", done_status="done", wave_channel="wave.w",
    ))
    assert sent, "nothing published"
    # Every status/event/result (task + wave mirror) carries reply_to = task id.
    assert all(reply_to == "TID" for _, _, _, reply_to in sent), sent
    results = [p for ch, kind, p, _ in sent if kind == "message"]
    assert len(results) == 1
    assert results[0]["status"] == status
    assert results[0]["task_id"] == "TID"
    assert set(results[0]) == {"status", "task_id", "result", "error"}
    assert {kind for _, kind, _, _ in sent} == {"status", "event", "message"}


def test_run_to_result_reports_errors():
    assert asyncio.run(run_to_result(_Boom(), "x", {}, "T")) == result_payload("T", error="kaboom")
    assert asyncio.run(run_to_result(_Echo(), "x", {}, "T")) == result_payload("T", result="echo: x")


def test_prompt_extraction_and_loop_guard():
    assert extract_prompt({"prompt": "a", "message": "b"}) == "a"
    assert extract_prompt({"message": "from telegram"}) == "from telegram"
    assert extract_prompt({"result": "x"}) is None
    assert is_task_result(result_payload("t", result="x"))
    assert not is_task_result({"prompt": "hi"})


# ── live ────────────────────────────────────────────────────────────


def _start_echo(url: str) -> tuple[str, subprocess.Popen]:
    identity = f"echo-{uuid.uuid4().hex[:6]}"
    proc = subprocess.Popen(
        [sys.executable, "echo_worker.py", "--identity", identity, "--nats-url", url],
        cwd=REPO_ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    return identity, proc


async def _wait_registered(nc, identity: str, sub) -> None:
    deadline = time.monotonic() + 15
    while True:
        msg = await sub.next_msg(timeout=max(deadline - time.monotonic(), 0.1))
        if json.loads(msg.data)["payload"].get("identity") == identity:
            return


def _run_live(scenario):
    import nats

    async def run():
        url = os.environ["NATS_URL"]
        nc = await nats.connect(url)
        reg = await nc.subscribe("hub.register")
        await nc.flush()
        started = time.monotonic()
        identity, proc = _start_echo(url)
        try:
            await _wait_registered(nc, identity, reg)
            return await scenario(nc, identity, started)
        finally:
            await nc.close()
            proc.terminate()
            proc.wait(timeout=5)

    return asyncio.run(run())


@live
def test_plain_dm_gets_dm_reply():
    """Rule 6: no task_channel → DM back to meta.from with reply_to = id."""

    async def scenario(nc, identity, _started):
        me = f"pytest-{uuid.uuid4().hex[:6]}"
        inbox = await nc.subscribe(f"channel.inbox.{me}")
        await nc.flush()
        task = make_envelope(me, identity, "chat.pytest", "message", {"message": "hey"})
        task_id = json.loads(task)["meta"]["id"]
        await nc.publish("hub.send.chat.pytest", task)
        msg = await inbox.next_msg(timeout=15)
        return task_id, me, json.loads(msg.data)

    task_id, me, env = _run_live(scenario)
    assert env["meta"]["kind"] == "message"
    assert env["meta"]["to"] == me
    assert env["meta"]["reply_to"] == task_id
    assert env["payload"] == result_payload(task_id, result="echo: yeh")


@live
def test_worker_listed_in_agents_within_3s():
    """run_worker registers immediately; hub-agents (agent.find) sees it fast."""

    async def scenario(nc, identity, started):
        req = json.dumps({"op": "agent.find", "params": {"capabilities": []}}).encode()
        while True:
            resp = json.loads((await nc.request("hub.api.agent.find", req, timeout=2)).data)
            agents = (resp.get("data") or {}).get("agents") or []
            if any(a.get("identity") == identity for a in agents):
                return time.monotonic() - started
            if time.monotonic() - started > 3:
                return None
            await asyncio.sleep(0.1)

    elapsed = _run_live(scenario)
    assert elapsed is not None and elapsed <= 3, "worker not listed within 3s"


def test_stop_signals_respect_existing_handlers():
    """install_stop_signals never overrides a handler the entrypoint set."""
    import signal

    from worker_runtime import install_stop_signals

    async def run():
        mine = lambda *_: None  # noqa: E731
        previous = signal.signal(signal.SIGHUP, mine)
        try:
            install_stop_signals(asyncio.current_task())
            assert signal.getsignal(signal.SIGHUP) is mine
        finally:
            asyncio.get_running_loop().remove_signal_handler(signal.SIGTERM)
            signal.signal(signal.SIGHUP, previous)

    asyncio.run(run())


@live
def test_sigterm_exits_cleanly():
    """SIGTERM → run_worker closes NATS and the process exits 0."""
    import nats

    async def run() -> int:
        url = os.environ["NATS_URL"]
        nc = await nats.connect(url)
        reg = await nc.subscribe("hub.register")
        await nc.flush()
        identity, proc = _start_echo(url)
        try:
            await _wait_registered(nc, identity, reg)
            proc.terminate()
            return proc.wait(timeout=5)
        finally:
            await nc.close()
            if proc.poll() is None:
                proc.kill()

    assert asyncio.run(run()) == 0
