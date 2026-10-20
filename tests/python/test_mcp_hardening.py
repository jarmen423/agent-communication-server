"""H3 hardening, MCP side (no NATS; in-memory bus from test_mcp_fakes):

- ``delegate_task`` cancels the task when it times out (H3.8)
- blocking timeouts are capped by the schema (H3.8)
- ``start_session`` / ``send_to_session`` return the turn's ``message_id`` and
  ``cancel_task`` accepts it (H3.7)
- a result from someone other than the delegated worker is ignored (C5)
"""
from __future__ import annotations

import asyncio
import json

from test_mcp_fakes import FakeWorker, _env, install

import hub_buffers  # noqa: E402
import nats_hub_mcp  # noqa: E402

H = nats_hub_mcp.HANDLERS


def test_delegate_task_timeout_sends_cancel(monkeypatch):
    async def run():
        bus = install(monkeypatch)
        w = await FakeWorker(bus, "w1", delay=30).start()
        res = await H["delegate_task"]({"to": "w1", "prompt": "x", "timeout": 0.2})
        assert not res["ok"] and "timed out" in res["error"]
        assert "cancel was sent" in res["error"]
        await asyncio.sleep(0.05)
        (task_id,) = list(hub_buffers.hub().tasks)
        assert [c["payload"] for c in w.control_seen] == [
            {"action": "cancel", "task_id": task_id}]
        st = await H["task_status"]({"task_id": task_id})
        assert st["data"]["cancel_requested"] and st["data"]["state"] == "cancelled"

    asyncio.run(run())


def test_blocking_timeouts_are_capped(monkeypatch):
    async def run():
        install(monkeypatch)
        for tool, args in (("delegate_task", {"to": "w", "prompt": "p"}),
                           ("wait_for_task", {"task_id": "t"}),
                           ("wait_for_message", {})):
            r = await H[tool]({**args, "timeout": 3601})
            assert not r["ok"] and "3600" in r["error"], (tool, r)
        r = await H["start_session"]({"worker": "w", "prompt": "p", "timeout": 10**9})
        assert not r["ok"] and "timeout" in r["error"]

    asyncio.run(run())


def test_forged_result_does_not_finish_a_delegated_task(monkeypatch):
    async def run():
        bus = install(monkeypatch)
        await FakeWorker(bus, "w1", delay=0.3).start()
        res = await H["delegate_async"]({"to": "w1", "prompt": "x"})
        task_id, channel = res["data"]["task_id"], res["data"]["task_channel"]
        forged = _env("mallory", channel, {"status": "done", "task_id": task_id,
                                           "result": "forged", "error": None},
                      kind="message", reply_to=task_id)
        await bus.publish(f"hub.pub.mallory.{channel}", json.dumps(forged).encode())
        await asyncio.sleep(0.05)
        assert hub_buffers.hub().tasks[task_id].state == "running"
        done = await H["wait_for_task"]({"task_id": task_id, "timeout": 2})
        assert done["data"]["result"]["result"] == "pong"

    asyncio.run(run())


class SessionWorker:
    """Holds every session turn until it is cancelled (§4.2)."""

    def __init__(self, bus, identity: str) -> None:
        self.bus, self.identity = bus, identity
        self.turns: dict[str, str] = {}  # turn id → session channel
        self.cancels: list[dict] = []

    async def start(self) -> "SessionWorker":
        await self.bus.subscribe(f"channel.inbox.{self.identity}", cb=self._on)
        return self

    async def _on(self, msg) -> None:
        env = json.loads(msg.data)
        meta, payload = env["meta"], env.get("payload") or {}
        if payload.get("action") == "session_start":
            channel = payload["session_channel"]
            self.turns[meta["id"]] = channel
            await self.bus.subscribe(f"channel.{channel}", cb=self._on)
        elif payload.get("action") == "session_send":
            self.turns[meta["id"]] = meta["channel"]
        elif meta.get("kind") == "control" and payload.get("action") == "cancel":
            self.cancels.append(env)
            turn = payload["task_id"]
            channel = self.turns.pop(turn, None)
            if channel is None:
                return
            res = _env(self.identity, channel, {"status": "cancelled", "task_id": turn,
                                                "result": None, "error": "cancelled"},
                       kind="message", reply_to=turn)
            await self.bus.publish(f"hub.pub.{self.identity}.{channel}",
                                   json.dumps(res).encode())


def test_cancel_task_accepts_session_turn_ids(monkeypatch):
    async def run():
        bus = install(monkeypatch)
        bus.api["session.create"] = lambda p: {"session_id": p["session_id"]}
        w = await SessionWorker(bus, "sw").start()
        started = await H["start_session"]({"worker": "sw", "prompt": "hi"})
        assert started["ok"], started
        sid, first = started["data"]["session_id"], started["data"]["message_id"]
        await asyncio.sleep(0.02)
        sent = await H["send_to_session"]({"session_id": sid, "message": "more"})
        assert sent["ok"] and sent["data"]["message_id"]
        turn = sent["data"]["message_id"]
        await asyncio.sleep(0.02)

        res = await H["cancel_task"]({"task_id": turn, "timeout": 2})
        assert res["ok"], res
        assert res["data"]["state"] == "cancelled" and res["data"]["session_id"] == sid
        ctrl = w.cancels[-1]
        assert ctrl["meta"]["to"] == "sw" and ctrl["meta"]["kind"] == "control"
        assert ctrl["payload"] == {"action": "cancel", "task_id": turn}

        res = await H["cancel_task"]({"task_id": first, "timeout": 2})
        assert res["ok"] and res["data"]["state"] == "cancelled"
        # Already finished: reported without a second cancel DM.
        again = await H["cancel_task"]({"task_id": first, "timeout": 2})
        assert again["ok"] and "already finished" in again["data"]["note"]
        assert len(w.cancels) == 2

    asyncio.run(run())


def test_send_to_session_finds_worker_from_session_record(monkeypatch):
    async def run():
        bus = install(monkeypatch)
        bus.api["session.get"] = lambda p: {"session": {"session_id": p["session_id"],
                                                        "worker": "sw"}}
        res = await H["send_to_session"]({"session_id": "s9", "message": "hi"})
        assert res["ok"] and "note" not in res["data"]
        turn = hub_buffers.hub().session_turns[res["data"]["message_id"]]
        assert (turn.worker, turn.channel) == ("sw", "session.s9")

        del bus.api["session.get"]  # unknown session: still sent, not cancellable
        res = await H["send_to_session"]({"session_id": "s10", "message": "hi"})
        assert res["ok"] and "can't be cancelled" in res["data"]["note"]

    asyncio.run(run())
