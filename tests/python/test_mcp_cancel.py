"""cancel_task — cancel contract (refocus-iteration-2.md §4.2) from the MCP side.

Unit tests run against an in-memory bus with a fake worker that honors the
contract (the real worker support lands in T3). The live test runs the same
fake worker over real NATS + the real router under scripts/dev/with_stack.sh.
"""
from __future__ import annotations

import asyncio
import json
import os
import uuid

import pytest

from test_mcp_fakes import FakeWorker, install

import hub_buffers  # noqa: E402
import hub_connection as conn  # noqa: E402
import nats_hub_mcp  # noqa: E402

H = nats_hub_mcp.HANDLERS  # validated handlers (same path as MCP + Hermes)


async def _delegate(worker: str) -> str:
    res = await H["delegate_async"]({"to": worker, "prompt": "long job"})
    assert res["ok"], res
    return res["data"]["task_id"]


def test_cancel_running_task_returns_cancelled_snapshot(monkeypatch):
    async def run():
        bus = install(monkeypatch)
        w = await FakeWorker(bus, "w1", delay=30).start()
        task_id = await _delegate("w1")
        await asyncio.sleep(0.02)  # worker picked it up
        assert task_id in {k for k in w.running}

        res = await H["cancel_task"]({"task_id": task_id, "timeout": 2})
        assert res["ok"], res
        snap = res["data"]
        assert snap["state"] == "cancelled"
        assert snap["cancel_requested"] is True
        assert snap["result"]["status"] == "cancelled"
        assert snap["result"]["task_id"] == task_id
        assert w.cancelled == [task_id]

        # The control DM follows §4.2.1 exactly.
        ctrl = w.control_seen[0]
        assert ctrl["meta"]["kind"] == "control"
        assert ctrl["meta"]["to"] == "w1"
        assert ctrl["meta"]["from"] == "t-orch"
        assert ctrl["payload"] == {"action": "cancel", "task_id": task_id}

        # task_status / wait_for_task agree afterwards.
        st = await H["task_status"]({"task_id": task_id})
        assert st["data"]["state"] == "cancelled"
        wt = await H["wait_for_task"]({"task_id": task_id, "timeout": 0.1})
        assert wt["ok"] and wt["data"]["state"] == "cancelled"

    asyncio.run(run())


def test_cancel_finished_task_sends_nothing(monkeypatch):
    async def run():
        bus = install(monkeypatch)
        w = await FakeWorker(bus, "w1", delay=0).start()
        task_id = await _delegate("w1")
        done = await H["wait_for_task"]({"task_id": task_id, "timeout": 2})
        assert done["data"]["state"] == "done"

        res = await H["cancel_task"]({"task_id": task_id})
        assert res["ok"]
        assert res["data"]["state"] == "done"
        assert "already finished" in res["data"]["note"]
        assert w.control_seen == []
        assert not any(e["meta"]["kind"] == "control" for e in bus.published)

    asyncio.run(run())


def test_cancel_worker_without_support_times_out_honestly(monkeypatch):
    """A worker that ignores cancel (pre-T3) → error with the live snapshot;
    the tracker is NOT marked cancelled locally."""

    async def run():
        bus = install(monkeypatch)
        w = await FakeWorker(bus, "old", delay=30, honors_cancel=False).start()
        task_id = await _delegate("old")
        await asyncio.sleep(0.02)
        res = await H["cancel_task"]({"task_id": task_id, "timeout": 0.1})
        assert not res["ok"]
        assert "no terminal result within 0.1s" in res["error"]
        assert "cancel_task again" in res["error"]
        assert res["data"]["state"] == "running"
        assert res["data"]["cancel_requested"] is True
        assert len(w.control_seen) == 1
        assert hub_buffers.hub().tasks[task_id].state == "running"

    asyncio.run(run())


def test_cancel_race_task_finishes_first(monkeypatch):
    """Worker finishes before seeing the cancel (§4.2.3: then it ignores it)."""

    async def run():
        bus = install(monkeypatch)
        await FakeWorker(bus, "w1", delay=0.05, honors_cancel=False).start()
        task_id = await _delegate("w1")
        res = await H["cancel_task"]({"task_id": task_id, "timeout": 2})
        assert res["ok"]
        assert res["data"]["state"] == "done"
        assert "before the cancel took effect" in res["data"]["note"]

    asyncio.run(run())


def test_delegate_task_reports_cancellation(monkeypatch):
    async def run():
        bus = install(monkeypatch)
        await FakeWorker(bus, "w1", delay=30).start()

        blocking = asyncio.ensure_future(
            H["delegate_task"]({"to": "w1", "prompt": "x", "timeout": 5}))
        await asyncio.sleep(0.05)
        (task_id,) = list(hub_buffers.hub().tasks)
        res = await H["cancel_task"]({"task_id": task_id})
        assert res["ok"] and res["data"]["state"] == "cancelled"
        out = await blocking
        assert not out["ok"] and "cancelled" in out["error"]

    asyncio.run(run())


def test_cancel_unknown_task_is_actionable(monkeypatch):
    async def run():
        install(monkeypatch)
        res = await H["cancel_task"]({"task_id": "nope"})
        assert not res["ok"]
        assert "only tracks tasks it delegated" in res["error"]
        assert "get_history" in res["error"]

    asyncio.run(run())


def test_cancel_task_argument_validation(monkeypatch):
    async def run():
        install(monkeypatch)
        r = await H["cancel_task"]({})
        assert not r["ok"] and "'task_id' is a required property" in r["error"]
        r = await H["cancel_task"]({"task_id": "x", "timeout": -1})
        assert not r["ok"] and "timeout" in r["error"]
        r = await H["cancel_task"]({"task_id": "x", "wait": 3})
        assert not r["ok"] and "'wait' was unexpected" in r["error"]

    asyncio.run(run())


def test_tracker_terminal_states():
    async def run():
        for status, want in (("done", "done"), ("error", "error"),
                             ("cancelled", "cancelled"), (None, "done")):
            tr = hub_buffers.TaskTracker("t", "task.t", "w")
            await tr.feed({"meta": {"kind": "message", "reply_to": "t"},
                           "payload": {"status": status, "task_id": "t"}})
            assert tr.state == want and tr.done.is_set()

    asyncio.run(run())


# ── Live: real NATS + router (with_stack.sh only) ─────────────────

live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh (nats-server + hub-server)",
)


@live
def test_cancel_task_live_through_router(monkeypatch):
    """A contract-conformant worker on real NATS: delegate a long task, then
    cancel_task returns the `cancelled` terminal snapshot via the router."""
    monkeypatch.setenv("NATS_HUB_IDENTITY", f"pytest-orch-{uuid.uuid4().hex[:6]}")
    worker_id = f"cancel-w-{uuid.uuid4().hex[:6]}"

    async def run():
        import nats

        wnc = await nats.connect(os.environ["NATS_URL"])
        running: dict[str, asyncio.Task] = {}

        async def send(env: dict) -> None:
            await wnc.publish(f"hub.send.{env['meta']['channel']}",
                              json.dumps(env).encode())

        def env(ch, payload, kind, reply_to):
            return {"meta": {"id": str(uuid.uuid4()), "from": worker_id,
                             "channel": ch, "to": None, "kind": kind,
                             "timestamp": conn.now(), "reply_to": reply_to},
                    "payload": payload}

        async def on_msg(msg):
            e = json.loads(msg.data)
            meta, p = e["meta"], e.get("payload") or {}
            if meta["kind"] == "control" and p.get("action") == "cancel":
                entry = running.pop(p.get("task_id"), None)
                if entry is None:
                    return
                task, ch = entry
                task.cancel()
                await send(env(ch, {"status": "cancelled", "task_id": p["task_id"],
                                    "result": None, "error": "cancelled"},
                               "message", p["task_id"]))
            elif meta["kind"] == "message" and p.get("task_channel"):
                ch = p["task_channel"]
                await send(env(ch, {"status": "working"}, "status", meta["id"]))
                running[meta["id"]] = (
                    asyncio.ensure_future(asyncio.sleep(300)), ch)

        await wnc.subscribe(f"channel.inbox.{worker_id}", cb=on_msg)
        await wnc.flush()
        try:
            res = await nats_hub_mcp.HANDLERS["delegate_async"](
                {"to": worker_id, "prompt": "sleep 300"})
            assert res["ok"], res
            task_id = res["data"]["task_id"]
            for _ in range(50):  # until the worker reports `working`
                st = await nats_hub_mcp.HANDLERS["task_status"]({"task_id": task_id})
                if st["data"]["last_status"] == "working":
                    break
                await asyncio.sleep(0.1)
            out = await nats_hub_mcp.HANDLERS["cancel_task"](
                {"task_id": task_id, "timeout": 10})
            assert out["ok"], out
            assert out["data"]["state"] == "cancelled"
            assert out["data"]["result"]["task_id"] == task_id
            assert running == {}
        finally:
            await wnc.close()
            await conn.close()
            hub_buffers._state = None

    asyncio.run(run())
