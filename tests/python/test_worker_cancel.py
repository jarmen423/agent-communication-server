"""Cancel contract (refocus-iteration-2.md §4.2) for every backend type:
CLIs get their process group killed, ACP agents get `session/cancel` (and a
kill if they ignore it), SDK threads are abandoned. The runtime reports one
terminal result with `status: "cancelled"`. No NATS needed."""
from __future__ import annotations

import asyncio
import inspect
import json
import os
import sys

import pytest

from fixtures.helpers import BIN, FAKE_ACP, MISBEHAVE, pid_alive, wait_dead, wait_for_file
from worker_backends.claude_code import ClaudeCodeBackend, ClaudeCodeConfig
from worker_backends.grok_acp import GrokAcpBackend
from worker_backends.headless_cli import HeadlessCliBackend, HeadlessCliSpec
from worker_backends.hermes_acp import HermesAcpBackend
from worker_backends.presets import kilo_spec, opencode_spec
from worker_backends.sdk_agent import SdkAgentBackend
from worker_backends.task_registry import TaskRegistry, TurnCancel, cancel_request
from worker_events import execute_with_events, result_payload, run_to_result


def _record():
    sent: list[tuple[str, str, dict, str | None]] = []

    async def publish(channel, kind, payload, reply_to=None):
        sent.append((channel, kind, payload, reply_to))

    async def publish_event(channel, event_type, data, reply_to=None):
        await publish(channel, "event", {"event_type": event_type, "data": data}, reply_to=reply_to)

    return sent, publish, publish_event


async def _run_and_cancel(backend, prompt: str, ready, ctx=None, after: float = 0.0):
    """Start a turn, wait for ``ready()`` (async, or sync in a thread), cancel it.
    Returns (outcome, published envelopes, seconds from cancel to result)."""
    sent, publish, publish_event = _record()
    cancel = TurnCancel()
    turn = asyncio.create_task(execute_with_events(
        publish=publish, publish_event_fn=publish_event, backend=backend,
        channel="task.c", prompt=prompt, ctx=ctx or {}, task_id="TID",
        working_status="working", done_status="done", cancel=cancel,
    ))
    if inspect.iscoroutinefunction(ready):
        info = await asyncio.wait_for(ready(), timeout=20)
    else:
        info = await asyncio.to_thread(ready)
    await asyncio.sleep(after)
    loop = asyncio.get_running_loop()
    t0 = loop.time()
    cancel.cancel("pytest")
    outcome = await asyncio.wait_for(turn, timeout=20)
    return outcome, sent, loop.time() - t0, info


def _assert_cancelled(sent):
    results = [p for _, kind, p, _ in sent if kind == "message"]
    assert results == [result_payload("TID", cancelled=True)], sent
    assert results[0]["status"] == "cancelled"
    assert all(reply_to == "TID" for *_, reply_to in sent)
    statuses = [p["status"] for _, kind, p, _ in sent if kind == "status"]
    assert statuses[-1] == "cancelled"
    errors = [p["data"] for _, kind, p, _ in sent
              if kind == "event" and p["event_type"] == "error"]
    assert errors and errors[-1]["cancelled"] is True and errors[-1]["by"] == "pytest"


# ── HeadlessCli: process-group kill ─────────────────────────────────


def test_cancel_kills_headless_cli_group(tmp_path):
    pidfile = tmp_path / "grandchild.pid"
    backend = HeadlessCliBackend(HeadlessCliSpec(
        binary=sys.executable, prompt_flag=None,
        base_argv=[str(MISBEHAVE), "sleep", str(pidfile)], timeout_sec=120))
    outcome, sent, took, grandchild = asyncio.run(_run_and_cancel(
        backend, "x", lambda: int(wait_for_file(pidfile, timeout=10))))
    assert outcome is None
    _assert_cancelled(sent)
    assert took < 10
    assert wait_dead([grandchild], timeout=5) == [], "CLI grandchild outlived the cancel"


def test_cancel_kills_claude_cli_group(tmp_path, monkeypatch):
    pidfile = tmp_path / "claude.json"
    monkeypatch.setenv("FAKE_CLAUDE_MODE", "sleep")
    monkeypatch.setenv("FAKE_CLAUDE_PIDFILE", str(pidfile))
    backend = ClaudeCodeBackend(ClaudeCodeConfig(claude_bin=str(BIN / "claude"), repo=tmp_path))
    outcome, sent, _, pids = asyncio.run(_run_and_cancel(
        backend, "long job", lambda: json.loads(wait_for_file(pidfile, timeout=10))))
    assert outcome is None
    _assert_cancelled(sent)
    assert wait_dead([pids["pid"], pids["child"]], timeout=5) == []


@pytest.mark.parametrize("make_spec", [kilo_spec, opencode_spec], ids=["kilo", "opencode"])
def test_cancel_kills_kilo_opencode_group(tmp_path, monkeypatch, make_spec):
    pidfile = tmp_path / "grandchild.pid"
    monkeypatch.setenv("FAKE_CLI_MODE", "sleep")
    monkeypatch.setenv("FAKE_CLI_PIDFILE", str(pidfile))
    name = "kilo" if make_spec is kilo_spec else "opencode"
    spec = make_spec(repo=tmp_path, **{f"{name}_bin": str(BIN / name)})
    outcome, sent, _, grandchild = asyncio.run(_run_and_cancel(
        HeadlessCliBackend(spec), "x", lambda: int(wait_for_file(pidfile, timeout=10))))
    assert outcome is None
    _assert_cancelled(sent)
    assert wait_dead([grandchild], timeout=5) == []


# ── ACP stdio: session/cancel, then kill ────────────────────────────


def _grok(tmp_path) -> GrokAcpBackend:
    return GrokAcpBackend(cwd=str(tmp_path), grok_cmd=str(FAKE_ACP), no_auto_update=False,
                          request_timeout_sec=60)


def test_acp_cancel_uses_session_cancel_and_keeps_agent(tmp_path):
    """A compliant agent ends the turn (stopReason cancelled); it stays up."""
    backend = _grok(tmp_path)

    async def streaming():
        while not (backend._proc and backend._chunks):
            await asyncio.sleep(0.02)

    async def run():
        try:
            result = await _run_and_cancel(backend, "hang", streaming)
            pid = backend._proc.pid
            text, _ = await backend.run("hello again", {})
            return result, pid, backend._proc.pid, text
        finally:
            await backend.close()

    (outcome, sent, took, _), pid_before, pid_after, text = asyncio.run(run())
    assert outcome is None
    _assert_cancelled(sent)
    assert took < 4, "session/cancel should end the turn well inside the kill grace"
    assert pid_before == pid_after, "a compliant agent must not be restarted"
    assert text.startswith("hello ")


def test_acp_cancel_kills_agent_that_ignores_it(tmp_path):
    backend = _grok(tmp_path)
    backend.cancel_grace_sec = 0.5

    async def run():
        try:
            turn = asyncio.ensure_future(backend.run("stubborn", {}))
            while not backend._chunks:
                await asyncio.sleep(0.02)
            pid = backend._proc.pid
            turn.cancel()
            with pytest.raises(asyncio.CancelledError):
                await turn
            dead = not pid_alive(pid)
            text, _ = await backend.run("after restart", {})  # restarts the agent
            return pid, dead, backend._proc.pid, text
        finally:
            await backend.close()

    pid, dead, new_pid, text = asyncio.run(run())
    assert dead, "agent that ignored session/cancel must be killed"
    assert new_pid != pid
    assert text.startswith("hello ")


def test_hermes_backend_speaks_acp_stdio_and_cancels(tmp_path):
    """HermesAcpBackend now runs on the shared ACP stdio plumbing (no `acp`
    package needed) and supports cancel."""
    backend = HermesAcpBackend(cwd=str(tmp_path), hermes_cmd=str(FAKE_ACP), model="m")

    async def run():
        try:
            text, ctx = await backend.run("hi", {})
            turn = asyncio.ensure_future(backend.run("hang", ctx))
            while not backend._chunks:
                await asyncio.sleep(0.02)
            turn.cancel()
            with pytest.raises(asyncio.CancelledError):
                await turn
            text2, ctx2 = await backend.run("again", ctx)
            return text, ctx, text2, ctx2
        finally:
            await backend.close()

    text, ctx, text2, ctx2 = asyncio.run(run())
    assert text.startswith("hello ") and text2.startswith("hello ")
    assert ctx["acp_session_id"] == ctx2["acp_session_id"], "same session reused"


# ── SdkAgent + runtime helpers ──────────────────────────────────────


def test_sdk_agent_cancel_calls_cancel_sync():
    import threading

    started, stop = threading.Event(), threading.Event()
    cancelled: list[bool] = []

    def run_sync(prompt, ctx):
        started.set()
        stop.wait(10)
        return "late", ctx

    def cancel_sync():
        cancelled.append(True)
        stop.set()

    backend = SdkAgentBackend(run_sync, cancel_sync=cancel_sync)
    outcome, sent, _, _ = asyncio.run(_run_and_cancel(backend, "x", lambda: started.wait(5)))
    assert outcome is None
    _assert_cancelled(sent)
    assert cancelled == [True]


def test_cancel_before_start_never_runs_backend():
    ran: list[str] = []

    class Backend:
        async def run(self, prompt, ctx):
            ran.append(prompt)
            return "x", ctx

    sent, publish, publish_event = _record()
    cancel = TurnCancel()
    cancel.cancel("queue")
    out = asyncio.run(execute_with_events(
        publish=publish, publish_event_fn=publish_event, backend=Backend(),
        channel="task.q", prompt="p", ctx={}, task_id="TID",
        working_status="working", done_status="done", cancel=cancel))
    assert out is None and ran == []
    assert [p for _, k, p, _ in sent if k == "message"] == [result_payload("TID", cancelled=True)]


def test_run_to_result_cancel_and_shutdown():
    class Slow:
        async def run(self, prompt, ctx):
            await asyncio.sleep(30)
            return "x", ctx

    async def cancelled_by_hub():
        cancel = TurnCancel()
        task = asyncio.ensure_future(run_to_result(Slow(), "p", {}, "T", cancel))
        await asyncio.sleep(0.05)
        cancel.cancel()
        return await task

    assert asyncio.run(cancelled_by_hub()) == result_payload("T", cancelled=True)

    async def shutdown():
        """A plain task.cancel() (worker shutdown) is not a §4.2 cancel."""
        sent, publish, publish_event = _record()
        task = asyncio.ensure_future(execute_with_events(
            publish=publish, publish_event_fn=publish_event, backend=Slow(),
            channel="task.s", prompt="p", ctx={}, task_id="T",
            working_status="working", done_status="done", cancel=TurnCancel()))
        await asyncio.sleep(0.05)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        return sent

    assert not [p for _, k, p, _ in asyncio.run(shutdown()) if k == "message"]


def test_cancel_request_and_registry():
    env = {"meta": {"kind": "control"}, "payload": {"action": "cancel", "task_id": "t1"}}
    assert cancel_request(env) == "t1"
    assert cancel_request({"meta": {"kind": "message"}, "payload": env["payload"]}) is None
    assert cancel_request({"meta": {"kind": "control"}, "payload": {"action": "cancel"}}) is None
    reg = TaskRegistry()
    handle = reg.register("t1")
    assert "t1" in reg and not reg.cancel("unknown")
    assert reg.cancel("t1", by="boss") and handle.requested and handle.by == "boss"
    reg.finish("t1")
    assert not reg.cancel("t1"), "finished tasks are ignored"


def test_result_payload_cancelled_shape():
    assert result_payload("t", cancelled=True) == {
        "status": "cancelled", "task_id": "t", "result": None, "error": "cancelled"}


@pytest.mark.skipif(not hasattr(os, "killpg"), reason="POSIX process groups")
def test_pid_helpers_sane():
    assert pid_alive(os.getpid())
