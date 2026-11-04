"""Session ownership must be recorded only after policy admission."""
import asyncio
import json
import os
import uuid

import nats
import pytest


@pytest.mark.asyncio
@pytest.mark.parametrize("busy", [False, True])
@pytest.mark.parametrize("prompt", [None, "work"])
@pytest.mark.parametrize("action", ["session_start", "session_send"])
async def test_live_rust_refusal_cannot_claim_session(busy, prompt, action, tmp_path):
    if os.environ.get("NATS_HUB_TEST_MANIFEST") != "1":
        pytest.skip("requires manifest writer stack")
    nc = await nats.connect(os.environ["NATS_URL"])
    worker = "admission-" + uuid.uuid4().hex
    channel = "task." + uuid.uuid4().hex
    async def api(caller, op, params):
        msg = await nc.request(f"hub.api.{caller}.{op}", json.dumps({"op": op, "params": params}).encode(), timeout=5)
        reply = json.loads(msg.data)
        assert reply["ok"], reply
        return reply.get("data") or {}
    await api("test-manifest", "manifest.put", {"record": {
        "identity": worker, "user": "alice", "owner_kind": "user", "agent": "echo",
        "accept": {"from": "owner"}, "rev": 9_000_000_000_000}})
    sub = await nc.subscribe("channel." + channel)
    slow_cat = tmp_path / "slow_cat.sh"  # --execute runs argv directly, not through a shell
    slow_cat.write_text("#!/bin/sh\nsleep 1\nexec cat\n")
    slow_cat.chmod(0o755)
    process = await asyncio.create_subprocess_exec("target/debug/hub-worker", "--identity", worker,
        "--execute", str(slow_cat), "--nats-url", os.environ["NATS_URL"],
        stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.DEVNULL)
    async def send(sender, payload):
        msg_id = uuid.uuid4().hex
        env = {"meta": {"id": msg_id, "from": sender, "kind": "message", "channel": channel,
            "timestamp": "2026-11-04T00:00:00Z", "origin": {"identity": sender, "user": sender}},
            "payload": {**payload, "task_channel": channel}}
        await nc.publish("channel.inbox." + worker, json.dumps(env).encode())
        await nc.flush()
        return msg_id
    async def receive(status):
        while True:
            env = json.loads((await sub.next_msg(timeout=5)).data)
            if env["payload"].get("status") == status:
                return env
    try:
        for _ in range(100):
            if (await api(worker, "agent.get", {"identity": worker})).get("agent"):
                break
            await asyncio.sleep(.05)
        else:
            pytest.fail("worker did not register")
        if busy:
            await send("alice", {"prompt": "hold"})
            await receive("working")
        payload = {"action": action, "session_id": "victim"}
        if prompt is not None:
            payload["prompt"] = prompt
        refused_id = await send("mallory", payload)
        refusal = await receive("error")
        assert refusal["payload"]["rejected"] is True
        assert refusal["payload"]["message_id"] == refused_id
        # A legitimate start must still own this id, even while work is queued.
        accepted_id = await send("alice", {"action": "session_start", "session_id": "victim", "prompt": "mine"})
        while True:
            done = await receive("done")
            if done["meta"].get("reply_to") == accepted_id:
                assert done["payload"]["result"].strip() == "mine"
                break
    finally:
        process.terminate()
        await asyncio.wait_for(process.wait(), 5)
        await nc.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("kind", ["message", "control", "human", None])
async def test_close_remains_available_after_policy_change(kind):
    from worker_backends.inbox import AcceptCheck
    async def unused(*args, **kwargs):
        raise AssertionError("close must not publish a refusal")
    check = AcceptCheck(unused, unused)
    check.record = {"user": "alice", "accept": {"from": "owner"}}
    assert await check.allows({"meta": {"kind": kind,
        "origin": {"identity": "bob", "user": "bob"}},
        "payload": {"action": "session_close", "prompt": "ignored"}})
