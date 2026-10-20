"""Durable delivery for the Python clients (W2-A; docs/DELIVERY.md).

The live tests boot a PRIVATE JetStream nats-server + `hub-server --durable`
(no storage), so they run the same under every suite mode. They skip when
the binaries are missing (build hub-server first: `make build`).
"""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "mcp_server"))

import hub_connection as conn  # noqa: E402
import hub_buffers  # noqa: E402
import hub_delivery  # noqa: E402
import hub_handlers  # noqa: E402
from worker_backends.inbox import InboxDispatcher  # noqa: E402
from worker_backends.task_registry import TaskRegistry  # noqa: E402


def _nats_bin() -> str | None:
    local = REPO_ROOT / ".tools/bin/nats-server"
    return str(local) if local.exists() else shutil.which("nats-server")


def _hub_bin() -> str | None:
    target = Path(os.environ.get("CARGO_TARGET_DIR", REPO_ROOT / "target"))
    p = target / "debug/hub-server"
    return str(p) if p.exists() else None


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _envelope(sender: str, channel: str, payload: dict, *, to: str | None = None,
              kind: str = "message", reply_to: str | None = None) -> dict:
    return {"meta": {"id": str(uuid.uuid4()), "from": sender, "channel": channel, "to": to,
                     "timestamp": "2026-01-01T00:00:00Z", "kind": kind,
                     "reply_to": reply_to}, "payload": payload}


@pytest.fixture(scope="module")
def durable_hub():
    nats_bin, hub_bin = _nats_bin(), _hub_bin()
    if not (nats_bin and hub_bin):
        pytest.skip("needs nats-server and a built hub-server")
    tmp = tempfile.mkdtemp(prefix="w2a-py-")
    port = _free_port()
    url = f"nats://127.0.0.1:{port}"
    nats = subprocess.Popen([nats_bin, "-a", "127.0.0.1", "-p", str(port), "-js",
                             "-sd", f"{tmp}/js"], stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL)
    args = [hub_bin, "--nats-url", url, "--db-path", "", "--durable"]
    if os.environ.get("NATS_HUB_REQUIRE_BOUND"):
        args.append("--require-bound-identity")
    hub = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        asyncio.run(_wait_routing(url))
        yield url
    finally:
        for p in (hub, nats):
            p.terminate()
            p.wait(timeout=10)
        shutil.rmtree(tmp, ignore_errors=True)


async def _wait_routing(url: str) -> None:
    import nats

    deadline = time.monotonic() + 30
    while True:
        try:
            nc = await nats.connect(url)
            break
        except Exception:  # noqa: BLE001
            assert time.monotonic() < deadline, "nats-server never came up"
            await asyncio.sleep(0.1)
    me = f"probe-{uuid.uuid4().hex[:6]}"
    sub = await nc.subscribe(f"channel.inbox.{me}")
    try:
        while time.monotonic() < deadline:
            env = _envelope(me, "probe", {}, to=me, kind="control")
            await nc.publish(f"hub.pub.{me}.probe", json.dumps(env).encode())
            try:
                await sub.next_msg(timeout=0.3)
                return
            except Exception:  # noqa: BLE001
                continue
        raise AssertionError("router never routed")
    finally:
        await nc.close()


async def _send(url: str, sender: str, env: dict) -> None:
    import nats

    nc = await nats.connect(url)
    await nc.publish(f"hub.pub.{sender}.{env['meta']['channel']}", json.dumps(env).encode())
    await nc.flush()
    await nc.close()


# ── AC3: redeliveries are handled once (no server) ───────────────────

class _Msg:
    def __init__(self, subject: str, env: dict) -> None:
        self.subject, self.data = subject, json.dumps(env).encode()


def test_dispatcher_drops_redelivered_envelopes():
    handled: list[str] = []

    async def handle(_msg, env):
        handled.append(env["meta"]["id"])

    async def report(_env):
        return None

    async def run():
        inbox = InboxDispatcher("channel.inbox.w", TaskRegistry(), handle, report)
        runner = asyncio.ensure_future(inbox.run())
        env = _envelope("boss", "task.x", {"prompt": "hi"}, to="w")
        for _ in range(3):  # at-least-once: the same meta.id three times
            await inbox.on_message(_Msg("channel.inbox.w", env))
        await asyncio.sleep(0.05)
        runner.cancel()
        await asyncio.gather(runner, return_exceptions=True)

    asyncio.run(run())
    assert len(handled) == 1


def test_seen_ids_window_and_task_channel_rule():
    seen = hub_delivery.SeenIds(cap=2)
    assert seen.first_sight("a") and not seen.first_sight("a")
    assert seen.first_sight("b") and seen.first_sight("c") and seen.first_sight("a")
    assert hub_delivery.task_channel_for("3f2a9c1e-0000-4000-8000-0") == "task.3f2a9c1e"


# ── AC2 + AC3: durable inbox through the helper ──────────────────────

def test_subscribe_inbox_gets_offline_dms_once(durable_hub):
    me = f"py-bob-{uuid.uuid4().hex[:6]}"

    async def run():
        import nats

        sent = []
        for i in range(3):  # bob has never connected
            env = _envelope("alice", "chat", {"n": i}, to=me)
            sent.append(env["meta"]["id"])
            await _send(durable_hub, "alice", env)
        # A redelivery: the same envelope stored a second time.
        nc = await nats.connect(durable_hub)
        js = nc.jetstream()
        dup = _envelope("alice", "chat", {"dup": True}, to=me)
        for n in range(2):
            await js.publish(f"channel.inbox.{me}", json.dumps(dup).encode(),
                             headers={"Nats-Msg-Id": f"t-{n}"})
        sent.append(dup["meta"]["id"])

        got: list[str] = []

        async def cb(msg):
            got.append(json.loads(msg.data)["meta"]["id"])

        bob = await nats.connect(durable_hub, inbox_prefix=f"_INBOX.{me}".encode())
        sub = await hub_delivery.subscribe_inbox(bob, me, cb)
        assert sub.durable
        deadline = time.monotonic() + 10
        while len(got) < 4 and time.monotonic() < deadline:
            await asyncio.sleep(0.05)
        await asyncio.sleep(0.5)  # nothing more arrives
        await sub.unsubscribe()
        await bob.close()
        await nc.close()
        return sent, got

    sent, got = asyncio.run(run())
    assert got == sent  # in order, the twin once


# ── AC2: the worker runtime picks up a DM sent while it was down ─────

def test_worker_runtime_runs_a_task_sent_while_it_was_offline(durable_hub):
    import nats

    worker = f"py-echo-{uuid.uuid4().hex[:6]}"

    async def run():
        nc = await nats.connect(durable_hub)
        task = _envelope("boss", "task.py1", {"prompt": "hello"}, to=worker)
        channel = hub_delivery.task_channel_for(task["meta"]["id"])
        task["meta"]["channel"] = channel
        task["payload"]["task_channel"] = channel
        sub = await nc.subscribe(f"channel.{channel}")
        await _send(durable_hub, "boss", task)  # the worker isn't running yet
        await asyncio.sleep(0.3)
        proc = subprocess.Popen(
            [sys.executable, "echo_worker.py", "--identity", worker, "--nats-url", durable_hub],
            cwd=REPO_ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                msg = await sub.next_msg(timeout=deadline - time.monotonic())
                env = json.loads(msg.data)
                if env["meta"]["kind"] == "message" and env["meta"]["reply_to"] == task["meta"]["id"]:
                    return env
            raise AssertionError("no result")
        finally:
            proc.terminate()
            proc.wait(timeout=10)
            await nc.close()

    result = asyncio.run(run())
    assert result["meta"]["from"] == worker
    assert result["payload"]["status"] == "done"


# ── AC2 + AC4: MCP inbox and wait_for_task across a reconnect ────────

def test_mcp_inbox_and_wait_for_task_survive_a_reconnect(durable_hub, monkeypatch):
    orch = f"py-orch-{uuid.uuid4().hex[:6]}"
    worker = f"py-w-{uuid.uuid4().hex[:6]}"
    monkeypatch.setenv("NATS_HUB_IDENTITY", orch)
    monkeypatch.setattr(conn, "NATS_URL", durable_hub)
    monkeypatch.setattr(hub_buffers, "_state", None)
    monkeypatch.setattr(conn, "_nc", None)

    async def run():
        # A DM that waits for the orchestrator (MCP server not running).
        early = _envelope(worker, "chat", {"hi": 1}, to=orch)
        await _send(durable_hub, worker, early)

        res = await hub_handlers.HANDLERS["delegate_async"]({"to": worker, "prompt": "p"})
        assert res["ok"], res
        task_id, channel = res["data"]["task_id"], res["data"]["task_channel"]
        assert channel == hub_delivery.task_channel_for(task_id)
        inbox = await hub_handlers.HANDLERS["read_inbox"]({})
        assert early["meta"]["id"] in json.dumps(inbox), inbox

        # Later, the MCP connection dies; the worker's result lands meanwhile.
        await asyncio.sleep(0.3)
        await conn._nc.close()
        result = _envelope(worker, channel, {"status": "done", "task_id": task_id,
                                             "result": "42", "error": None},
                           reply_to=task_id)
        await _send(durable_hub, worker, result)
        await asyncio.sleep(0.3)

        done = await hub_handlers.HANDLERS["wait_for_task"]({"task_id": task_id, "timeout": 10})
        await conn.close()
        return done

    done = asyncio.run(run())
    assert done["ok"], done
    assert done["data"]["state"] == "done", done
    assert done["data"]["result"]["result"] == "42"
