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
@pytest.mark.parametrize("broadcast", [False, True])
@pytest.mark.parametrize("backend", ["python", "rust"])
@pytest.mark.parametrize("origin", [None, {"identity":"bob", "user":"bob", "owner_kind":"user"},
    {"identity":"hub-orchestrator", "on_behalf_of":{"identity":"alice"}}])
async def test_live_inbox_bypass_is_refused(backend, origin, broadcast):
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
        "accept":{"from":"tenant" if origin and origin.get("identity") == "hub-orchestrator" else "owner"}, "rev":9_000_000_000_000}})
    assert response["ok"]
    sub = await nc.subscribe("channel." + channel)
    command = ([sys.executable, "echo_worker.py"] if backend == "python" else
               ["target/debug/hub-worker", "--execute", "cat"])
    if broadcast and backend == "python":
        command = [sys.executable, "-c",
            "import asyncio,sys; from echo_worker import echo_run; "
            "from worker_runtime import WorkerConfig,run_worker; "
            "from worker_backends.sdk_agent import SdkAgentBackend; "
            "asyncio.run(run_worker(WorkerConfig(identity=sys.argv[1],nats_url=sys.argv[2],"
            "broadcast_channel=sys.argv[3],backend=SdkAgentBackend(echo_run))))",
            identity, url, "broadcast-" + identity]
    elif broadcast:
        command += ["--channel", "broadcast-" + identity]
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
        await nc.publish(("channel.broadcast-" if broadcast else "channel.inbox.") + identity, json.dumps({"meta": {
            "id":task_id, "from":"intruder", "origin":origin, "channel":channel,
            "kind":"message", "timestamp":"2026-11-04T00:00:00Z"},
            "payload":{"action":"cancel", "prompt":"must not execute", "task_channel":channel, "task_id":"victim"}}).encode())
        message = await sub.next_msg(timeout=3)
        terminal = json.loads(message.data)["payload"]
        if origin and origin.get("identity") == "hub-orchestrator":
            # The status precedes the successful terminal on a permitted task.
            while terminal.get("status") not in ("done", "error"):
                terminal = json.loads((await sub.next_msg(timeout=3)).data)["payload"]
            assert terminal["status"] == "done"
        else:
            assert terminal["rejected"] is True
            assert terminal["status"] == "error"
        if terminal.get("rejected"):
            assert terminal["task_id"] == "victim"
            assert terminal["message_id"] == task_id
            assert terminal["refused_sender"] == (origin or {}).get("identity")
        else:
            assert terminal["task_id"] == task_id
    finally:
        process.terminate()
        await asyncio.wait_for(process.wait(), 5)
        await nc.close()

@pytest.mark.asyncio
async def test_mcp_tracker_only_trusts_router_rejection(monkeypatch):
    import sys
    from pathlib import Path
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "mcp_server"))
    from hub_primitives import TaskTracker
    import hub_connection
    monkeypatch.setattr(hub_connection, "identity", lambda: "sender")
    tracker = TaskTracker("id", "task.id", "worker")
    def terminal(sender):
        return {"meta":{"from":sender,"kind":"message","reply_to":"id"},
                "payload":{"task_id":"id", "message_id":"id", "refused_sender":"sender", "status":"error","error":"denied","rejected":True}}
    await tracker._feed(terminal("stranger"))
    assert not tracker.done.is_set()
    attack = terminal("hub-server")
    attack["payload"]["refused_sender"] = "attacker"
    await tracker._feed(attack)
    assert not tracker.done.is_set()
    attack["payload"]["refused_sender"] = "sender"
    attack["payload"]["message_id"] = "another-send"
    await tracker._feed(attack)
    assert not tracker.done.is_set()
    await tracker._feed(terminal("hub-server"))
    assert tracker.done.is_set()
    assert "refused" in tracker.result["payload"]["error"]

@pytest.mark.asyncio
@pytest.mark.parametrize("source", ["router", "worker"])
async def test_mcp_session_rejection_returns_refused(monkeypatch, source):
    import sys
    from pathlib import Path
    from types import SimpleNamespace
    sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "mcp_server"))
    import hub_handlers
    monkeypatch.setattr(hub_handlers.conn, "identity", lambda: "sender")
    from hub_primitives import RingBuffer
    inbox = RingBuffer(10)
    session = RingBuffer(10)
    async def started():
        pass
    async def buffer(_):
        return SimpleNamespace(buf=session)
    async def start(_):
        await (inbox if source == "router" else session).put({"meta":{"from":"hub-server" if source == "router" else "worker","reply_to":"id"},
                         "payload":{"action":"rejected","rejected":True,"error":"denied","message_id":"id","refused_sender":"sender"}})
        return {"ok":True,"data":{"session_id":"sid","message_id":"id"}}
    monkeypatch.setattr(hub_handlers,"hub",lambda:SimpleNamespace(
        inbox=SimpleNamespace(buf=inbox),ensure_started=started,session_buffer=buffer))
    closed = []
    async def api(*args):
        closed.append(args)
        return {"ok": True}
    monkeypatch.setattr(hub_handlers.conn, "api_request", api)
    monkeypatch.setitem(hub_handlers.SESSION_HANDLERS,"start_session",start)
    response = await hub_handlers._start_session_checked({"worker":"worker"})
    assert not response["ok"] and "refused" in response["error"]
    assert closed == [("session.update_status", {"session_id": "sid", "status": "closed"})]

@pytest.mark.parametrize("origin,allowed", [
    ({"identity":"hub-orchestrator", "on_behalf_of":{"identity":"alice"}}, True),
    ({"identity":"hub-orchestrator"}, False),
    ({"identity":"bob"}, False),
])
def test_tenant_wave_origin(origin, allowed):
    assert policy_allows({"accept":{"from":"tenant"}}, origin) is allowed


@pytest.mark.asyncio
@pytest.mark.parametrize("resumed", [False, True])
async def test_session_turn_policy_after_resume(resumed):
    import asyncio
    import json
    from types import SimpleNamespace
    from worker_backends.inbox import InboxDispatcher
    from worker_backends.task_registry import TaskRegistry
    from worker_backends.session_resume import resume_all
    handled = []
    async def handle(msg, env):
        handled.append(env["payload"]["action"])
    async def publish(*args, **kwargs):
        pass
    async def request(op, params):
        return {"ok": True, "data": {"sessions": [
            {"session_id":"sid", "worker":"worker", "orchestrator":"bob"}]}}
    dispatcher = InboxDispatcher("channel.inbox.worker", TaskRegistry(), handle, publish)
    dispatcher.accept_check = AcceptCheck(request, publish)
    dispatcher.accept_check.record = {"user":"alice", "accept":{"from":"owner"}}
    callbacks = []
    if resumed:
        async def resume(*args):
            callbacks.append(dispatcher.on_session_message)
        await resume_all(request, "worker", resume)
        assert callbacks
    else:
        callbacks.append(dispatcher.on_session_message)
    runner = asyncio.create_task(dispatcher.run())
    try:
        for action in ("session_send", "session_close"):
            env = {"meta":{"id":action,"from":"bob","kind":"control" if action == "session_close" else "message",
                           "origin":{"identity":"bob","user":"bob"}},
                   "payload":{"action":action,"session_id":"sid","prompt":"do work"}}
            await callbacks[0](SimpleNamespace(subject="channel.session.sid", data=json.dumps(env).encode()))
        await asyncio.sleep(.02)
        assert handled == ["session_close"]
    finally:
        runner.cancel()
        await asyncio.gather(runner, return_exceptions=True)

@pytest.mark.asyncio
async def test_live_wave_to_tenant_worker():
    import asyncio
    import json
    import os
    import sys
    import uuid
    import nats
    if os.environ.get("NATS_HUB_TEST_MANIFEST") != "1":
        pytest.skip("requires manifest writer stack")
    nc = await nats.connect(os.environ["NATS_URL"])
    suffix = uuid.uuid4().hex
    worker, owner, wave = "tenant-" + suffix, "owner-" + suffix, "wave-" + suffix
    async def api(caller, op, params):
        msg = await nc.request(f"hub.api.{caller}.{op}", json.dumps({"op":op,"params":params}).encode(), timeout=5)
        reply = json.loads(msg.data)
        assert reply["ok"], reply
        return reply.get("data") or {}
    for identity in (worker, owner):
        await api("test-manifest", "manifest.put", {"record": {
            "identity":identity,"user":"alice","owner_kind":"user","agent":"echo",
            "accept":{"from":"tenant"},"rev":9_000_000_000_000}})
    process = await asyncio.create_subprocess_exec(sys.executable,"echo_worker.py","--identity",worker,
        "--nats-url",os.environ["NATS_URL"],stdout=asyncio.subprocess.DEVNULL,stderr=asyncio.subprocess.DEVNULL)
    try:
        for _ in range(100):
            if (await api(worker,"agent.get",{"identity":worker})).get("agent"):
                break
            await asyncio.sleep(.05)
        else:
            pytest.fail("worker did not register")
        await api(owner,"wave.create",{"wave":{"wave_id":wave,"goal":"hello","status":"pending",
            "orchestrator":owner,"created_at":"2026-11-04T00:00:00Z"},
            "tasks":[{"task_id":"t","worker":worker,"goal":"hello"}]})
        await api(owner,"wave.spawn",{"wave_id":wave,"timeout_secs":15})
        state = {}
        for _ in range(100):
            state = await api(owner,"wave.status",{"wave_id":wave})
            if state["wave"]["status"] in ("completed","failed"):
                break
            await asyncio.sleep(.05)
        assert state["wave"]["status"] == "completed", state
    finally:
        process.terminate()
        await asyncio.wait_for(process.wait(),5)
        await nc.close()

@pytest.mark.asyncio
async def test_live_resumed_session_refuses_turn_but_allows_close():
    import asyncio
    import json
    import os
    import sys
    import uuid
    import nats
    if os.environ.get("NATS_HUB_TEST_MANIFEST") != "1":
        pytest.skip("requires manifest writer stack")
    nc = await nats.connect(os.environ["NATS_URL"])
    suffix = uuid.uuid4().hex
    worker, bob, sid = "resume-" + suffix, "bob-" + suffix, "session-" + suffix
    channel = "session." + sid
    async def api(caller, op, params):
        msg = await nc.request(f"hub.api.{caller}.{op}", json.dumps({"op":op,"params":params}).encode(), timeout=5)
        reply = json.loads(msg.data)
        assert reply["ok"], reply
        return reply.get("data") or {}
    for identity, user in ((worker,"alice"), (bob,"bob")):
        await api("test-manifest", "manifest.put", {"record": {
            "identity":identity,"user":user,"owner_kind":"user","agent":"echo",
            "accept":{"from":"owner"},"rev":9_000_000_000_000}})
    await api(bob,"session.create",{"session_id":sid,"orchestrator":bob,"worker":worker,
        "status":"active","created_at":"2026-11-04T00:00:00Z","updated_at":"2026-11-04T00:00:00Z",
        "metadata":{},"backend_ctx":{}})
    sub = await nc.subscribe("channel." + channel)
    process = await asyncio.create_subprocess_exec(sys.executable,"echo_worker.py","--identity",worker,
        "--nats-url",os.environ["NATS_URL"],stdout=asyncio.subprocess.PIPE,stderr=asyncio.subprocess.DEVNULL,
        env={**os.environ,"PYTHONUNBUFFERED":"1"})
    async def resumed():
        while line := await process.stdout.readline():
            if b"resumed session" in line:
                return
        pytest.fail("worker exited before resuming")
    try:
        await asyncio.wait_for(resumed(),5)
        for action in ("session_send","session_close"):
            env = {"meta":{"id":uuid.uuid4().hex,"from":bob,"channel":channel,
                           "kind":"control" if action == "session_close" else "message","timestamp":"2026-11-04T00:00:00Z"},
                   "payload":{"action":action,"session_id":sid,"message":"must not execute"}}
            await nc.publish(f"hub.pub.{bob}.{channel}",json.dumps(env).encode())
            await nc.flush()
            message = json.loads((await sub.next_msg(timeout=3)).data)
            assert message["meta"]["from"] == bob  # the command's echo
            if action == "session_send":
                with pytest.raises(nats.errors.TimeoutError):
                    await sub.next_msg(timeout=.25)
            else:
                closed = json.loads((await sub.next_msg(timeout=3)).data)
                assert closed["meta"]["from"] == worker
                assert closed["payload"]["status"] == "closed"
    finally:
        process.terminate()
        await asyncio.wait_for(process.wait(),5)
        await nc.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["message", "control", "status", "event"])
async def test_session_start_cannot_bypass_policy_with_kind(kind):
    async def unused(*args, **kwargs):
        pass
    check = AcceptCheck(unused, unused)
    check.record = {"user":"alice", "accept":{"from":"owner"}}
    assert not await check.allows({"meta":{"kind":kind},
                                   "payload":{"action":"session_start", "prompt":"run"}})

@pytest.mark.asyncio
@pytest.mark.parametrize("enforced", [False, True])
async def test_self_enforcement_flag_and_non_dict_payload(enforced):
    async def request(*args):
        return {"ok": True, "data": {"enforced": enforced, "record": {
            "user": "alice", "accept": {"from": "owner"}}}}
    async def publish(*args, **kwargs):
        pass
    check = AcceptCheck(request, publish)
    await check.refresh()
    assert await check.allows({"meta": {}, "payload": ["bad"]}) is not enforced

@pytest.mark.asyncio
async def test_cancel_action_on_work_message_is_not_exempt():
    sent = []
    async def unused(*args):
        pass
    async def publish(*args, **kwargs):
        sent.append(args[2])
    check = AcceptCheck(unused, publish)
    check.record = {"user": "alice", "accept": {"from": "owner"}}
    assert not await check.allows({"meta": {"id": "bob-id", "kind": "message",
        "origin": {"identity": "bob", "user": "bob"}}, "payload": {
        "action": "cancel", "prompt": "…", "task_channel": "task.x"}})
    assert sent[0]["refused_sender"] == "bob"
    assert sent[0]["message_id"] == "bob-id"

@pytest.mark.asyncio
@pytest.mark.parametrize("sender", ["worker", "hub-server"])
async def test_third_party_worker_refusal_does_not_end_delegation(monkeypatch, sender):
    import hub_connection
    from hub_primitives import TaskTracker
    monkeypatch.setattr(hub_connection, "identity", lambda: "alice")
    tracker = TaskTracker("alice-id", "task.alice-id", "worker")
    await tracker._feed({"meta": {"from": sender, "kind": "message", "reply_to": "alice-id"},
        "payload": {"status": "error", "task_id": "alice-id", "rejected": True,
                    "refused_sender": "bob", "message_id": "bob-id"}})
    assert not tracker.done.is_set()

@pytest.mark.asyncio
async def test_session_start_refusal_is_correlated_status():
    sent = []
    async def unused(*args):
        pass
    async def publish(*args, **kwargs):
        sent.append(args)
    check = AcceptCheck(unused, publish)
    check.record = {"user": "alice", "accept": {"from": "owner"}}
    assert not await check.allows({"meta": {"id": "start-id", "kind": "message",
        "origin": {"identity": "bob", "user": "bob"}}, "payload": {
        "action": "session_start", "session_channel": "session.sid", "session_id": "sid"}})
    channel, kind, terminal = sent[0]
    assert (channel, kind) == ("session.sid", "status")
    assert terminal["message_id"] == "start-id" and terminal["refused_sender"] == "bob"
