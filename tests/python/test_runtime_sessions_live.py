"""Live session lifecycle on a real worker (needs scripts/dev/with_stack.sh):

- an opaque wave-style session id (``<wave_id>--<task_id>``, C4) works;
- ``session_close`` frees the session: its channel subscription is gone, so
  a later broadcast ``session_send`` is not answered (H3.12);
- a ``session_send`` for a session the worker doesn't hold gets an ``error``
  result on the session channel instead of silence (H3.6).
"""
from __future__ import annotations

import asyncio
import json
import os
import sys

from fixtures.live import fake_env, live, run, start_worker, stop_worker, uniq, wait_result
from worker_runtime import make_envelope

SENDER = "pytest"


async def _pub(nc, env: bytes) -> str:
    meta = json.loads(env)["meta"]
    await nc.publish(f"hub.pub.{SENDER}.{meta['channel']}", env)
    await nc.flush()
    return meta["id"]


async def _no_result(sub, task_id: str, within: float) -> bool:
    try:
        await wait_result(sub, task_id, within=within)
    except Exception:  # noqa: BLE001 - nats TimeoutError: nothing correlated arrived
        return True
    return False


@live
def test_session_close_frees_session_and_unknown_session_errors(tmp_path):
    import nats

    async def scenario():
        nc = await nats.connect(os.environ["NATS_URL"])
        worker = uniq("sess-echo")
        argv = [sys.executable, "echo_worker.py", "--identity", worker,
                "--nats-url", os.environ["NATS_URL"]]
        proc = await start_worker(nc, worker, argv, fake_env(), tmp_path / "worker.log")
        sid = f"{uniq('wave')}--t1"
        channel = f"session.{sid}"
        sub = await nc.subscribe(f"channel.{channel}")
        await nc.flush()
        try:
            start = make_envelope(SENDER, worker, channel, "message", {
                "action": "session_start", "session_id": sid,
                "session_channel": channel, "prompt": "abc"})
            first = await wait_result(sub, await _pub(nc, start), within=15)

            send = make_envelope(SENDER, None, channel, "message", {
                "action": "session_send", "session_id": sid, "message": "xyz"})
            second = await wait_result(sub, await _pub(nc, send), within=15)

            close = make_envelope(SENDER, None, channel, "message", {
                "action": "session_close", "session_id": sid})
            await _pub(nc, close)
            await asyncio.sleep(1.0)

            after = make_envelope(SENDER, None, channel, "message", {
                "action": "session_send", "session_id": sid, "message": "late"})
            unanswered = await _no_result(sub, await _pub(nc, after), within=2.5)

            dm = make_envelope(SENDER, worker, channel, "message", {
                "action": "session_send", "session_id": sid, "message": "dm"})
            unknown = await wait_result(sub, await _pub(nc, dm), within=15)
            return first, second, unanswered, unknown, proc.poll()
        finally:
            stop_worker(proc)
            await nc.close()

    first, second, unanswered, unknown, exit_code = run(scenario())
    assert first["payload"]["status"] == "done" and first["payload"]["result"] == "echo: cba"
    assert second["payload"]["result"] == "echo: zyx"
    assert unanswered, "a closed session must not keep its channel subscription"
    assert unknown["payload"]["status"] == "error"
    assert "unknown session" in unknown["payload"]["error"]
    assert exit_code is None
