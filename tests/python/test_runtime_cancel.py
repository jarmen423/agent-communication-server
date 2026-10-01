"""Cancel over the hub (refocus-iteration-2.md §4.2), Python runtime path:
a real `claude_worker.py` (fake `claude` that hangs) gets a task, then a
`kind=control {"action":"cancel"}` DM. Live: needs with_stack.sh."""
from __future__ import annotations

import asyncio
import json
import os
import sys

from fixtures.helpers import BIN, pid_alive, wait_dead, wait_for_file
from fixtures.live import (
    fake_env,
    live,
    run,
    send_cancel,
    send_task,
    start_worker,
    stop_worker,
    uniq,
    wait_result,
)
from worker_events import result_payload


async def _claude_worker(nc, tmp_path, pidfile):
    identity = uniq("cancel-claude")
    argv = [sys.executable, "claude_worker.py", "--identity", identity,
            "--repo", str(tmp_path), "--nats-url", os.environ["NATS_URL"],
            "--claude-bin", str(BIN / "claude")]
    env = fake_env(FAKE_CLAUDE_MODE="sleep", FAKE_CLAUDE_PIDFILE=str(pidfile))
    return identity, await start_worker(nc, identity, argv, env, tmp_path / "worker.log")


@live
def test_cancel_running_and_queued_tasks(tmp_path):
    import nats

    pidfile = tmp_path / "claude.json"

    async def scenario():
        nc = await nats.connect(os.environ["NATS_URL"])
        identity, proc = await _claude_worker(nc, tmp_path, pidfile)
        try:
            running_id, running_sub = await send_task(nc, "pytest", identity, "long job")
            pids = json.loads(await asyncio.to_thread(wait_for_file, pidfile, 15))
            queued_id, queued_sub = await send_task(nc, "pytest", identity, "queued job")
            await asyncio.sleep(0.3)  # let it land in the worker's queue

            # Unknown task: ignored (no reply, worker keeps running the task).
            await send_cancel(nc, "pytest", identity, "no-such-task")
            # Queued task: cancelled without ever running.
            await send_cancel(nc, "pytest", identity, queued_id)
            queued = await wait_result(queued_sub, queued_id, within=10)
            assert all(pid_alive(p) for p in pids.values()), "unknown/queued cancel hit the running task"

            loop = asyncio.get_running_loop()
            t0 = loop.time()
            await send_cancel(nc, "pytest", identity, running_id)
            running = await wait_result(running_sub, running_id, within=10)
            return pids, queued, running, loop.time() - t0, proc.poll()
        finally:
            stop_worker(proc)
            await nc.close()

    pids, queued, running, took, exit_code = run(scenario())
    assert queued["payload"] == result_payload(queued["meta"]["reply_to"], cancelled=True)
    assert running["payload"]["status"] == "cancelled"
    assert running["payload"] == result_payload(running["meta"]["reply_to"], cancelled=True)
    assert took < 10
    assert exit_code is None, "the worker must survive a cancel"
    assert wait_dead([pids["pid"], pids["child"]], timeout=5) == [], "CLI group not killed"
