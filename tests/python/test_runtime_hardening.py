"""H3 hardening (.planning/refocus-2/HARDENING.md) — runtime pieces, no NATS:

- ``run_verify_cmd``: >1 MB output, timeout, cwd, cancel during verify (H3.1)
- exactly one terminal result even when publishing it fails (H3.5)
- inbox dispatcher: cancels only from the task's sender (C5), re-dispatched
  ``session_start`` aliases the in-flight turn (C3), session-channel turns are
  registered on arrival (H3.7)
"""
from __future__ import annotations

import asyncio
import json
import sys
from types import SimpleNamespace

import pytest

from fixtures.helpers import pid_alive, wait_dead, wait_for_file
from worker_backends.inbox import InboxDispatcher
from worker_backends.task_registry import TaskRegistry, TurnCancel
from worker_events import execute_with_events, result_payload, run_verify_cmd
from worker_runtime import make_envelope

INBOX = "channel.inbox.w"


def _record():
    sent: list[tuple[str, str, dict, str | None]] = []

    async def publish(channel, kind, payload, reply_to=None):
        sent.append((channel, kind, payload, reply_to))

    async def publish_event(channel, event_type, data, reply_to=None):
        await publish(channel, "event", {"event_type": event_type, "data": data}, reply_to=reply_to)

    return sent, publish, publish_event


class Quick:
    async def run(self, prompt, ctx):
        return f"ok:{prompt}", ctx


def _results(sent):
    return [p for _, kind, p, _ in sent if kind == "message"]


# ── run_verify_cmd ──────────────────────────────────────────────────


def test_verify_more_than_1mb_output_does_not_hang():
    big = f"{sys.executable} -c \"import sys; sys.stdout.write('x' * 3_000_000); sys.stderr.write('e' * 500_000)\""
    asyncio.run(asyncio.wait_for(run_verify_cmd(big, timeout=30), timeout=40))


def test_verify_failure_quotes_bounded_output_tail():
    cmd = (f"{sys.executable} -c \"import sys; sys.stdout.write('a' * 2_000_000 + 'THE-END');"
           " sys.exit(3)\"")
    with pytest.raises(RuntimeError) as err:
        asyncio.run(run_verify_cmd(cmd, timeout=30))
    msg = str(err.value)
    assert "exit 3" in msg and msg.endswith("THE-END")
    assert len(msg) < 10_000


def test_verify_timeout_kills_its_process_group(tmp_path):
    pidfile = tmp_path / "bg.pid"
    cmd = f"sleep 60 & echo $! > {pidfile}; wait"
    loop_time = []

    async def run():
        t0 = asyncio.get_running_loop().time()
        with pytest.raises(RuntimeError, match="timed out"):
            await run_verify_cmd(cmd, timeout=0.5)
        loop_time.append(asyncio.get_running_loop().time() - t0)

    asyncio.run(run())
    assert loop_time[0] < 10
    assert wait_dead([int(wait_for_file(pidfile))]) == [], "verify grandchild outlived the timeout"


def test_verify_background_child_holding_pipe_is_bounded(tmp_path):
    """The command exits but leaves a daemon holding stdout: don't wait for it."""
    pidfile = tmp_path / "bg.pid"
    cmd = f"sleep 60 & echo $! > {pidfile}; exit 0"

    async def run():
        await asyncio.wait_for(run_verify_cmd(cmd, timeout=60), timeout=15)

    asyncio.run(run())
    assert wait_dead([int(wait_for_file(pidfile))]) == []


def test_verify_runs_in_cwd(tmp_path):
    asyncio.run(run_verify_cmd(f'test "$(pwd -P)" = "{tmp_path.resolve()}"', cwd=str(tmp_path)))
    with pytest.raises(RuntimeError):
        asyncio.run(run_verify_cmd(f'test "$(pwd -P)" = "{tmp_path.resolve()}"', cwd="/"))


def test_cancel_during_verify_reports_cancelled_and_kills_verify(tmp_path):
    pidfile = tmp_path / "verify.pid"
    sent, publish, publish_event = _record()
    cancel = TurnCancel()

    async def run():
        turn = asyncio.ensure_future(execute_with_events(
            publish=publish, publish_event_fn=publish_event, backend=Quick(),
            channel="wave.w.task.t", prompt="p", ctx={}, task_id="TID",
            working_status="working", done_status="idle",
            verify_cmd=f"echo $$ > {pidfile}; exec sleep 60",
            cancel=cancel, verify_cwd=str(tmp_path), verify_timeout=120))
        pid = int(await asyncio.to_thread(wait_for_file, pidfile, 10))
        cancel.cancel("boss")
        out = await asyncio.wait_for(turn, timeout=15)
        return pid, out

    pid, out = asyncio.run(run())
    assert out is None
    assert _results(sent) == [result_payload("TID", cancelled=True)]
    events = [p["event_type"] for _, k, p, _ in sent if k == "event"]
    assert "completed" not in events and "milestone" not in events
    assert not pid_alive(pid) or wait_dead([pid]) == [], "verify_cmd survived the cancel"


def test_verify_timeout_is_an_error_result(tmp_path):
    sent, publish, publish_event = _record()
    out = asyncio.run(execute_with_events(
        publish=publish, publish_event_fn=publish_event, backend=Quick(),
        channel="c", prompt="p", ctx={}, task_id="TID", working_status="working",
        done_status="idle", verify_cmd="sleep 30", verify_timeout=0.3))
    assert out is None
    [res] = _results(sent)
    assert res["status"] == "error" and "timed out" in res["error"]


# ── exactly one terminal result ─────────────────────────────────────


def test_failed_result_publish_never_sends_a_second_result():
    sent, publish, publish_event = _record()

    async def flaky(channel, kind, payload, reply_to=None):
        await publish(channel, kind, payload, reply_to)
        if kind == "status" and payload.get("status") == "done":
            raise ConnectionError("bus gone")

    out = asyncio.run(execute_with_events(
        publish=flaky, publish_event_fn=publish_event, backend=Quick(),
        channel="c", prompt="p", ctx={}, task_id="TID",
        working_status="working", done_status="done"))
    assert out is None
    assert _results(sent) == [result_payload("TID", result="ok:p")]
    assert not [p for _, k, p, _ in sent if k == "event" and p["event_type"] == "error"]


def test_cancel_after_result_does_not_add_a_cancelled_result():
    sent, publish, publish_event = _record()
    cancel = TurnCancel()

    async def cancelling(channel, kind, payload, reply_to=None):
        await publish(channel, kind, payload, reply_to)
        if kind == "message":
            cancel.cancel("late")  # arrives while the result is being published

    asyncio.run(execute_with_events(
        publish=cancelling, publish_event_fn=publish_event, backend=Quick(),
        channel="c", prompt="p", ctx={}, task_id="TID",
        working_status="working", done_status="done", cancel=cancel))
    assert _results(sent) == [result_payload("TID", result="ok:p")]


# ── dispatcher: C5 sender check, C3 alias, session-channel turns ────


def _msg(sender, kind, payload, *, subject=INBOX, channel="inbox.w", to="w"):
    data = make_envelope(sender, to, channel, kind, payload)
    return SimpleNamespace(subject=subject, data=data), json.loads(data)["meta"]["id"]


class Harness:
    """InboxDispatcher with a handler that blocks each turn until released."""

    def __init__(self) -> None:
        self.registry = TaskRegistry()
        self.started: list[str] = []
        self.release = asyncio.Event()
        self.reported: list[str] = []
        self.inbox = InboxDispatcher(INBOX, self.registry, self.handle, self.report)
        self.runner = asyncio.ensure_future(self.inbox.run())

    async def handle(self, msg, envelope):
        task_id = envelope["meta"]["id"]
        self.started.append(task_id)
        handle = self.registry.get(task_id)
        turn = asyncio.ensure_future(self.release.wait())
        handle.attach(turn)
        try:
            await turn
        except asyncio.CancelledError:
            if not handle.requested:
                raise

    async def report(self, envelope):
        self.reported.append(envelope["meta"]["id"])

    async def settle(self):
        for _ in range(5):
            await asyncio.sleep(0.01)

    async def close(self):
        self.runner.cancel()
        await asyncio.gather(self.runner, return_exceptions=True)


def test_cancel_from_third_party_is_ignored():
    async def run():
        h = Harness()
        task, tid = _msg("boss", "message", {"prompt": "x", "task_channel": "task.1"})
        await h.inbox.on_message(task)
        await h.settle()
        handle = h.registry.get(tid)
        mallory, _ = _msg("mallory", "control", {"action": "cancel", "task_id": tid})
        await h.inbox.on_message(mallory)
        await h.settle()
        ignored = not handle.requested
        boss, _ = _msg("boss", "control", {"action": "cancel", "task_id": tid})
        await h.inbox.on_message(boss)
        await h.settle()
        honoured = handle.requested and handle.by == "boss"
        await h.close()
        return ignored, honoured

    ignored, honoured = asyncio.run(run())
    assert ignored, "a cancel from someone other than the task's sender must be ignored"
    assert honoured


def test_queued_task_cancel_needs_the_sender_too():
    async def run():
        h = Harness()
        first, _ = _msg("boss", "message", {"prompt": "a", "task_channel": "task.a"})
        queued, qid = _msg("boss", "message", {"prompt": "b", "task_channel": "task.b"})
        await h.inbox.on_message(first)
        await h.inbox.on_message(queued)
        await h.settle()
        await h.inbox.on_message(_msg("eve", "control", {"action": "cancel", "task_id": qid})[0])
        await h.settle()
        before = list(h.reported)
        await h.inbox.on_message(_msg("boss", "control", {"action": "cancel", "task_id": qid})[0])
        await h.settle()
        await h.close()
        return before, h.reported

    before, after = asyncio.run(run())
    assert before == [] and len(after) == 1


def test_redispatched_session_start_aliases_running_turn():
    async def run():
        h = Harness()
        start = {"action": "session_start", "session_id": "w1--t1", "prompt": "go",
                 "channel": "wave.w1.task.t1"}
        first, id1 = _msg("hub-orchestrator", "message", start)
        again, id2 = _msg("hub-orchestrator", "message", start)
        await h.inbox.on_message(first)
        await h.settle()
        await h.inbox.on_message(again)
        await h.settle()
        started_once = list(h.started)
        aliased = h.registry.get(id2) is h.registry.get(id1)
        # Cancel by the newest dispatch id (C2) stops the one running turn.
        await h.inbox.on_message(_msg("hub-orchestrator", "control",
                                      {"action": "cancel", "task_id": id2})[0])
        await h.settle()
        cancelled = h.registry.get(id1) is None  # turn ended → both ids released
        third, id3 = _msg("hub-orchestrator", "message", start)
        await h.inbox.on_message(third)  # turn over: a new start runs again
        await h.settle()
        h.release.set()
        await h.settle()
        await h.close()
        return id1, id3, started_once, aliased, cancelled, h.started, id2

    id1, id3, started_once, aliased, cancelled, started, id2 = asyncio.run(run())
    assert started_once == [id1], "a re-dispatch while running must not start a second run"
    assert aliased and cancelled
    assert started == [id1, id3] and id2 not in started


def test_session_channel_send_is_cancellable_while_queued():
    async def run():
        h = Harness()
        start, _ = _msg("boss", "message", {"action": "session_start", "session_id": "s1",
                                            "prompt": "first"})
        await h.inbox.on_message(start)
        await h.settle()
        send, sid = _msg("boss", "message",
                         {"action": "session_send", "session_id": "s1", "message": "next"},
                         subject="channel.session.s1", channel="session.s1", to=None)
        noise, nid = _msg("w", "message", {"status": "done", "task_id": "x"},
                          subject="channel.session.s1", channel="session.s1", to=None)
        await h.inbox.on_session_message(send)
        await h.inbox.on_session_message(noise)  # our own result echo: not work
        await h.settle()
        registered = sid in h.registry and nid not in h.registry
        await h.inbox.on_message(_msg("boss", "control", {"action": "cancel", "task_id": sid})[0])
        await h.settle()
        requested = h.registry.get(sid).requested
        await h.close()
        return registered, requested

    registered, requested = asyncio.run(run())
    assert registered and requested


def test_cancel_on_session_channel_is_not_honoured():
    """Cancels are DMs: a cancel broadcast on a session channel is dropped."""
    async def run():
        h = Harness()
        task, tid = _msg("boss", "message", {"prompt": "x", "task_channel": "task.1"})
        await h.inbox.on_message(task)
        await h.settle()
        bc, _ = _msg("boss", "control", {"action": "cancel", "task_id": tid},
                     subject="channel.session.s1", channel="session.s1", to=None)
        await h.inbox.on_session_message(bc)
        await h.inbox.on_message(bc)
        await h.settle()
        requested = h.registry.get(tid).requested
        await h.close()
        return requested

    assert asyncio.run(run()) is False
