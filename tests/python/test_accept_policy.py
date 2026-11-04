"""Worker policy checks reject inbox bypasses before execution."""
import pytest
from worker_backends.inbox import policy_allows, task_channel, AcceptCheck

@pytest.mark.parametrize("origin,allowed", [(None, False), ({}, False),
    ({"user": "alice", "owner_kind": "user"}, True),
    ({"user": "bob", "owner_kind": "user"}, False),
    ({"identity":"hub-orchestrator", "user":"alice", "on_behalf_of":{"user":"bob"}}, False)])
def test_owner(origin, allowed):
    assert policy_allows({"identity":"worker", "user":"alice", "owner_kind":"user"}, origin) is allowed

@pytest.mark.parametrize("channel,allowed", [("inbox.victim",False),("task.a",True),("task.*",False),("wave.w.task.t",True)])
def test_channel(channel, allowed):
    assert task_channel(channel) is allowed

@pytest.mark.asyncio
async def test_refused_without_origin_and_refresh_error_keeps_record():
    published = []
    async def request(*args):
        raise OSError("transient")
    async def publish(*args, **kwargs):
        published.append((args, kwargs))
    check = AcceptCheck(request, publish)
    check.record = {"identity":"worker", "user":"alice", "owner_kind":"user"}
    await check.refresh()
    assert not await check.allows({"meta":{"id":"id", "channel":"task.a"},"payload":{"prompt":"do work"}})
    assert published[0][0][2]["rejected"] is True
    published.clear()
    assert not await check.allows({"meta":{"id":"id"},"payload":{"task_channel":"inbox.victim"}})
    assert not published

@pytest.mark.asyncio
@pytest.mark.parametrize("backend", ["python", "rust"])
@pytest.mark.parametrize("origin", [None, {"identity":"bob", "user":"bob", "owner_kind":"user"}])
async def test_live_inbox_bypass_is_refused(backend, origin):
    import asyncio
    import json
    import os
    import sys
    import uuid
    import nats
    if os.environ.get("NATS_HUB_TEST_MANIFEST") != "1":
        pytest.skip("requires manifest writer stack")
    url = os.environ["NATS_URL"]
    nc = await nats.connect(url)
    identity = "check-" + uuid.uuid4().hex
    channel = "task." + uuid.uuid4().hex
    async def request(caller, op, params):
        msg = await nc.request(f"hub.api.{caller}.{op}", json.dumps({"op":op,"params":params}).encode(), timeout=5)
        return json.loads(msg.data)
    response = await request("test-manifest", "manifest.put", {"record": {
        "identity":identity, "owner_kind":"user", "user":"alice", "agent":"echo",
        "accept":{"from":"owner"}, "rev":9_000_000_000_000}})
    assert response["ok"]
    sub = await nc.subscribe("channel." + channel)
    command = ([sys.executable, "echo_worker.py"] if backend == "python" else
               ["target/debug/hub-worker", "--execute", "cat"])
    process = await asyncio.create_subprocess_exec(*command, "--identity", identity,
        "--nats-url", url, stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.DEVNULL)
    try:
        for _ in range(100):
            response = await request(identity, "agent.get", {"identity":identity})
            if response.get("data", {}).get("agent"):
                break
            assert process.returncode is None
            await asyncio.sleep(.05)
        else:
            pytest.fail("worker did not register")
        # Direct delivery bypasses router stamping: missing origin must refuse.
        task_id = uuid.uuid4().hex
        await nc.publish("channel.inbox." + identity, json.dumps({"meta": {
            "id":task_id, "from":"intruder", "origin":origin, "channel":channel,
            "kind":"message", "timestamp":"2026-11-04T00:00:00Z"},
            "payload":{"prompt":"must not execute", "task_channel":channel}}).encode())
        message = await sub.next_msg(timeout=3)
        terminal = json.loads(message.data)["payload"]
        assert terminal["rejected"] is True
        assert terminal["status"] == "error"
        assert terminal["task_id"] == task_id
    finally:
        process.terminate()
        await asyncio.wait_for(process.wait(), 5)
        await nc.close()

@pytest.mark.asyncio
async def test_mcp_tracker_only_trusts_router_rejection():
    import sys
    from pathlib import Path
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "mcp_server"))
    from hub_primitives import TaskTracker
    tracker = TaskTracker("id", "task.id", "worker")
    def terminal(sender):
        return {"meta":{"from":sender,"kind":"message","reply_to":"id"},
                "payload":{"task_id":"id", "status":"error","error":"denied","rejected":True}}
    await tracker._feed(terminal("stranger"))
    assert not tracker.done.is_set()
    await tracker._feed(terminal("hub-server"))
    assert tracker.done.is_set()
    assert "refused" in tracker.result["payload"]["error"]

@pytest.mark.asyncio
async def test_mcp_session_rejection_returns_refused(monkeypatch):
    import sys
    from pathlib import Path
    from types import SimpleNamespace
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "mcp_server"))
    import hub_handlers
    from hub_primitives import RingBuffer
    inbox = RingBuffer(10)
    session = RingBuffer(10)
    async def started():
        pass
    async def buffer(_):
        return SimpleNamespace(buf=session)
    async def start(_):
        await inbox.put({"meta":{"from":"hub-server","reply_to":"id"},
                         "payload":{"action":"rejected","error":"denied"}})
        return {"ok":True,"data":{"session_id":"sid","message_id":"id"}}
    monkeypatch.setattr(hub_handlers,"hub",lambda:SimpleNamespace(
        inbox=SimpleNamespace(buf=inbox),ensure_started=started,session_buffer=buffer))
    monkeypatch.setitem(hub_handlers.SESSION_HANDLERS,"start_session",start)
    response = await hub_handlers._start_session_checked({"worker":"worker"})
    assert not response["ok"] and "refused" in response["error"]
