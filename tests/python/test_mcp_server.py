"""Tests for the unified nats-hub MCP server (mcp_server/).

Unit tests always run. The live test only runs under
scripts/dev/with_stack.sh (NATS_URL + hub-server available): it boots
echo_worker.py and drives delegate_async → wait_for_task end-to-end,
exercising the reply contract (refocus.md §6) through the real router.
"""
from __future__ import annotations

import asyncio
import json
import os
import subprocess
import sys
import uuid
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "mcp_server"))

import hub_buffers  # noqa: E402
import hub_connection as conn  # noqa: E402
import hub_handlers  # noqa: E402
import nats_hub_mcp  # noqa: E402


def _env(task_id: str, kind: str = "message", payload=None,
         reply_to=None, sender="echo-1") -> dict:
    meta = {"id": str(uuid.uuid4()), "from": sender, "kind": kind,
            "channel": f"task.{task_id[:8]}"}
    if reply_to is not None:
        meta["reply_to"] = reply_to
    return {"meta": meta, "payload": payload or {}}


# ── Reply-contract result matching ───────────────────────────────


def test_is_task_result_via_reply_to():
    env = _env("t1", reply_to="t1", payload={"result": "ok"})
    assert hub_buffers.is_task_result(env, "t1")


def test_is_task_result_via_payload_task_id():
    # Worker runtime stamps reply_to=<channel>, so payload.task_id is
    # the correlation that works today (refocus.md §6.5).
    env = _env("t1", reply_to="task.abc123",
               payload={"task_id": "t1", "result": "ok"})
    assert hub_buffers.is_task_result(env, "t1")


def test_is_task_result_rejects_status_and_events():
    status = _env("t1", kind="status",
                  payload={"task_id": "t1", "status": "working"})
    event = _env("t1", kind="event",
                 payload={"task_id": "t1", "event_type": "started"})
    assert not hub_buffers.is_task_result(status, "t1")
    assert not hub_buffers.is_task_result(event, "t1")


def test_is_task_result_rejects_unrelated_message():
    env = _env("t1", payload={"text": "hello"})
    assert not hub_buffers.is_task_result(env, "t1")
    other = _env("t2", reply_to="t2")
    assert not hub_buffers.is_task_result(other, "t1")


# ── RingBuffer ───────────────────────────────────────────────────


def test_ringbuffer_seq_tail_since():
    async def run():
        buf = hub_buffers.RingBuffer(10)
        for i in range(5):
            await buf.put({"payload": {"n": i}})
        assert buf.last_seq == 5
        assert [it["env"]["payload"]["n"] for it in buf.tail(2)] == [3, 4]
        assert [it["env"]["payload"]["n"] for it in buf.since(3)] == [3, 4]
        assert buf.since(5) == []
        assert buf.since(0, limit=2) == [
            {"seq": 4, "env": {"payload": {"n": 3}}},
            {"seq": 5, "env": {"payload": {"n": 4}}},
        ]
    asyncio.run(run())


def test_ringbuffer_bounded():
    async def run():
        buf = hub_buffers.RingBuffer(3)
        for i in range(6):
            await buf.put({"payload": {"n": i}})
        assert buf.last_seq == 6
        assert [it["env"]["payload"]["n"] for it in buf.tail(10)] == [3, 4, 5]
        assert buf.since(3)[0]["seq"] == 4  # oldest dropped
    asyncio.run(run())


def test_ringbuffer_wait_for_timeout_and_match():
    async def run():
        buf = hub_buffers.RingBuffer(10)
        assert await buf.wait_for(lambda e: True, timeout=0.05) is None

        async def delayed_put():
            await asyncio.sleep(0.05)
            await buf.put({"meta": {"from": "bob"}, "payload": {}})
            await buf.put({"meta": {"from": "alice"}, "payload": {}})

        t = asyncio.ensure_future(delayed_put())
        it = await buf.wait_for(
            lambda e: e["meta"]["from"] == "alice", timeout=2)
        assert it is not None and it["env"]["meta"]["from"] == "alice"
        assert it["seq"] == 2
        await t
    asyncio.run(run())


# ── TaskTracker ──────────────────────────────────────────────────


def test_task_tracker_states():
    async def run():
        tr = hub_buffers.TaskTracker("t1", "task.t1", "echo-1")
        await tr.feed(_env("t1", kind="status",
                           payload={"status": "working"}))
        assert tr.state == "running" and tr.last_status == "working"
        await tr.feed(_env("t1", kind="event",
                           payload={"event_type": "started"}))
        await tr.feed(_env("t1", payload={"text": "unrelated"}))
        assert tr.other_messages.last_seq == 1
        await tr.feed(_env("t1", payload={"task_id": "t1",
                                        "status": "done",
                                        "result": "r"}))
        assert tr.state == "done" and tr.done.is_set()
        snap = tr.snapshot()
        assert snap["result"]["result"] == "r"
        assert len(snap["events"]) == 2
    asyncio.run(run())


def test_task_tracker_error_result():
    async def run():
        tr = hub_buffers.TaskTracker("t1", "task.t1", "echo-1")
        await tr.feed(_env("t1", payload={"task_id": "t1",
                                        "status": "error",
                                        "error": "boom"}))
        assert tr.state == "error" and tr.done.is_set()
    asyncio.run(run())


# ── Identity ─────────────────────────────────────────────────────


def test_identity_required(monkeypatch):
    monkeypatch.delenv("NATS_HUB_IDENTITY", raising=False)
    with pytest.raises(RuntimeError, match="NATS_HUB_IDENTITY"):
        conn.identity()


def test_identity_from_env(monkeypatch):
    monkeypatch.setenv("NATS_HUB_IDENTITY", "orch-test")
    assert conn.identity() == "orch-test"
    env = conn.envelope("ch", {"x": 1})
    assert env["meta"]["from"] == "orch-test"
    assert env["meta"]["kind"] == "message"
    assert env["meta"]["channel"] == "ch"


def test_check_from_arg(monkeypatch):
    monkeypatch.setenv("NATS_HUB_IDENTITY", "orch-test")
    assert conn.check_from_arg({}) is None
    assert conn.check_from_arg({"from": "orch-test"}) is None
    assert "does not match" in conn.check_from_arg({"from": "mallory"})
    assert "does not match" in conn.check_from_arg({"orchestrator": "mallory"})


# ── Surface + drift ──────────────────────────────────────────────


def test_all_tools_have_handlers_and_schemas_are_clean():
    names = {t.name for t in nats_hub_mcp.TOOLS}
    assert names == set(hub_handlers.HANDLERS)
    for required in ("delegate_async", "task_status", "wait_for_task",
                     "read_inbox", "wait_for_message", "session_replies",
                     "check_providers"):
        assert required in names
    for t in nats_hub_mcp.TOOLS:
        props = t.inputSchema.get("properties", {})
        assert "orchestrator" not in props or t.name == "list_sessions"
        # `from` survives only as a *filter* (get_history, wait_for_message)
        if t.name not in ("get_history", "wait_for_message"):
            assert "from" not in props, f"{t.name} still takes `from`"


def test_plugin_server_copies_in_sync():
    r = subprocess.run(
        ["bash", "scripts/dev/sync_plugins.sh", "--check"],
        cwd=REPO_ROOT, capture_output=True, text=True)
    assert r.returncode == 0, r.stderr or r.stdout


# ── Live round-trip (with_stack.sh only) ─────────────────────────

live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh (nats-server + hub-server)",
)


@live
def test_delegate_async_wait_for_task_round_trip(monkeypatch):
    """delegate_async → wait_for_task against a real echo worker through
    the real router; the result must satisfy the reply contract."""
    monkeypatch.setenv("NATS_HUB_IDENTITY", f"pytest-orch-{uuid.uuid4().hex[:6]}")
    worker_id = f"echo-{uuid.uuid4().hex[:6]}"

    async def run():
        worker = subprocess.Popen(
            [sys.executable, "echo_worker.py", "--identity", worker_id,
             "--nats-url", os.environ["NATS_URL"]],
            cwd=REPO_ROOT,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            await asyncio.sleep(1.5)  # let the worker subscribe to its inbox

            res = await hub_handlers.HANDLERS["delegate_async"]({
                "to": worker_id, "prompt": "abc",
            })
            assert res["ok"], res
            task_id = res["data"]["task_id"]
            assert res["data"]["task_channel"].startswith("task.")

            snap = await hub_handlers.HANDLERS["task_status"]({
                "task_id": task_id})
            assert snap["ok"] and snap["data"]["state"] in ("running", "done")

            done = await hub_handlers.HANDLERS["wait_for_task"]({
                "task_id": task_id, "timeout": 30})
            assert done["ok"], done
            d = done["data"]
            assert d["state"] == "done"
            assert d["result"]["result"] == "echo: cba"
            # Contract: correlated to the task envelope id.
            assert d["result"]["task_id"] == task_id

            inbox = await hub_handlers.HANDLERS["read_inbox"]({"limit": 10})
            assert inbox["ok"]
            assert inbox["data"]["identity"] == os.environ["NATS_HUB_IDENTITY"]
        finally:
            worker.terminate()
            worker.wait(timeout=5)
            await conn.close()
            hub_buffers._state = None

    asyncio.run(run())
