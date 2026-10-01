"""Tests for the wave MCP tools + wave/session API ops.

Unit-level handler checks always run (api_request stubbed). Live tests run
under scripts/dev/with_stack.sh: the hub-server orchestrator must drive a
wave end-to-end through the MCP handlers alone — no in-process spawn loop.
"""
from __future__ import annotations

import asyncio
import json
import os
import sys
import uuid
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "mcp_server"))

import hub_connection as conn  # noqa: E402
import hub_handlers  # noqa: E402
import hub_queries  # noqa: E402


def _wave_args(goal="g", tasks=None):
    return {"goal": goal, "from": "pytest-orch",
            "tasks": tasks or [
                {"task_id": "t1", "worker": "w1", "goal": "a",
                 "write_scope": ["src/a"], "dependencies": []},
                {"task_id": "t2", "worker": "w2", "goal": "b",
                 "write_scope": ["src/b"], "dependencies": ["t1"]},
            ]}


# ── Unit: handlers are thin passthroughs over hub.api ────────────


def test_spawn_wave_calls_api(monkeypatch):
    calls = []

    async def fake_api(op, params):
        calls.append((op, params))
        return {"ok": True, "data": {"wave": {"status": "running"}}}

    monkeypatch.setattr(conn, "api_request", fake_api)
    res = asyncio.run(hub_handlers.HANDLERS["spawn_wave"](
        {"wave_id": "w9", "timeout": 60}))
    assert res["ok"]
    assert calls == [("wave.spawn", {"wave_id": "w9", "timeout_secs": 60})]


def test_spawn_wave_surfaces_api_error(monkeypatch):
    async def fake_api(op, params):
        return {"ok": False, "error": "wave 'wX' is 'completed'"}

    monkeypatch.setattr(conn, "api_request", fake_api)
    res = asyncio.run(hub_handlers.HANDLERS["spawn_wave"]({"wave_id": "wX"}))
    assert res["ok"] is False
    assert "completed" in res["error"]


def test_cancel_wave_calls_api(monkeypatch):
    calls = []

    async def fake_api(op, params):
        calls.append((op, params))
        return {"ok": True, "data": {"wave": {"status": "cancelled"}}}

    monkeypatch.setattr(conn, "api_request", fake_api)
    res = asyncio.run(hub_handlers.HANDLERS["cancel_wave"]({"wave_id": "w9"}))
    assert res["ok"]
    assert calls == [("wave.cancel", {"wave_id": "w9"})]


def test_create_wave_is_atomic(monkeypatch):
    """create_wave must send wave + tasks in ONE wave.create call."""
    calls = []

    async def fake_api(op, params):
        calls.append((op, params))
        return {"ok": True, "data": {"wave_id": params["wave"]["wave_id"],
                                     "tasks": [t["task_id"] for t in params["tasks"]]}}

    monkeypatch.setattr(conn, "api_request", fake_api)
    monkeypatch.setenv("NATS_HUB_IDENTITY", "pytest-orch")
    res = asyncio.run(hub_handlers.HANDLERS["create_wave"](_wave_args()))
    assert res["ok"], res
    assert len(calls) == 1 and calls[0][0] == "wave.create"
    wave, tasks = calls[0][1]["wave"], calls[0][1]["tasks"]
    assert wave["status"] == "pending"
    assert {t["task_id"] for t in tasks} == {"t1", "t2"}
    # The API op takes WaveTaskInput — no storage fields leak in.
    assert "created_at" not in tasks[0] and "status" not in tasks[0]


# ── Live: the orchestrator drives a wave end-to-end ──────────────

live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh (nats-server + hub-server)",
)


def _worker_env(task_id: str, wave_id: str, sender: str,
                event_type: str, data: dict) -> dict:
    return {
        "meta": {"id": str(uuid.uuid4()), "from": sender,
                 "channel": f"wave.{wave_id}.task.{task_id}", "to": None,
                 "timestamp": conn.now(), "kind": "event", "reply_to": None},
        "payload": {"event_type": event_type, "data": data},
    }


async def _publish_as(nc, sender: str, channel: str, env: dict) -> None:
    """Publish as `sender` on its bound subject (contract §4.1). The router
    stamps meta.from from the subject, so a test that wants an event to count
    as the worker's must actually send it as the worker."""
    await nc.publish(f"hub.pub.{sender}.{channel}", json.dumps(env).encode())
    await nc.flush()


@live
def test_mcp_wave_tools_drive_orchestrator(monkeypatch):
    """create → spawn → worker events → wave completes; and a forged
    foreign-sender completion is ignored along the way."""
    monkeypatch.setenv("NATS_HUB_IDENTITY", "pytest-orch")
    w1 = f"w1-{uuid.uuid4().hex[:6]}"

    async def run():
        import nats

        # Subscribe w1's inbox before spawn so the dispatch DM isn't missed.
        probe = await nats.connect(os.environ["NATS_URL"])
        inbox = await probe.subscribe(f"channel.inbox.{w1}")
        await probe.flush()
        await asyncio.sleep(0.3)

        res = await hub_handlers.HANDLERS["create_wave"](_wave_args(
            tasks=[{"task_id": "ta", "worker": w1, "goal": "first",
                    "write_scope": ["src/a"], "dependencies": [],
                    "verify_cmd": "true"},
                   {"task_id": "tb", "worker": "w2-x", "goal": "second",
                    "write_scope": ["src/b"], "dependencies": ["ta"]}]))
        assert res["ok"], res
        wave_id = res["data"]["wave_id"]
        assert set(res["data"]["tasks"]) == {"ta", "tb"}

        # Cycle rejection through the same tool.
        cyc = await hub_handlers.HANDLERS["create_wave"](_wave_args(
            tasks=[{"task_id": "x", "worker": w1, "goal": "g",
                    "write_scope": ["x"], "dependencies": ["y"]},
                   {"task_id": "y", "worker": w1, "goal": "g",
                    "write_scope": ["y"], "dependencies": ["x"]}]))
        assert cyc["ok"] is False and "cycle" in cyc["error"]

        res = await hub_handlers.HANDLERS["spawn_wave"]({"wave_id": wave_id})
        assert res["ok"], res
        assert res["data"]["wave"]["status"] == "running"

        # session_start DM arrives for ta on w1's inbox.
        env = None
        deadline = asyncio.get_running_loop().time() + 15
        while asyncio.get_running_loop().time() < deadline:
            try:
                msg = await inbox.next_msg(timeout=1)
            except Exception:
                continue
            env = json.loads(msg.data)
            if env.get("payload", {}).get("action") == "session_start":
                break
            env = None
        assert env is not None, "no session_start DM for ta"
        assert env["payload"]["session_id"] == "ta"
        assert env["payload"]["wave_id"] == wave_id
        assert env["payload"]["verify_cmd"] == "true"

        # A foreign sender cannot complete ta.
        forged = _worker_env("ta", wave_id, "mallory", "completed",
                             {"result": "forged"})
        await _publish_as(probe, "mallory", f"wave.{wave_id}.task.ta", forged)
        await asyncio.sleep(0.8)
        snap = await hub_queries._wave_status({"wave_id": wave_id})
        assert snap["ok"]
        states = {t["task_id"]: t["status"] for t in snap["data"]["tasks"]}
        assert states["ta"] == "running"

        # The real worker verifies then completes; tb dispatches and its
        # worker completes → wave completes.
        await _publish_as(probe, w1, f"wave.{wave_id}.task.ta",
                          _worker_env("ta", wave_id, w1, "milestone",
                                      {"name": "verify_passed"}))
        await _publish_as(probe, w1, f"wave.{wave_id}.task.ta",
                          _worker_env("ta", wave_id, w1, "completed",
                                      {"result": "ok"}))

        snap = None
        deadline = asyncio.get_running_loop().time() + 15
        while asyncio.get_running_loop().time() < deadline:
            snap = await hub_queries._wave_status({"wave_id": wave_id})
            wave = snap["data"]["wave"]
            tasks = {t["task_id"]: t for t in snap["data"]["tasks"]}
            if wave["status"] == "running" and tasks["tb"]["status"] == "running":
                break
            await asyncio.sleep(0.25)
        assert tasks["tb"]["status"] == "running", f"tb never dispatched: {snap}"

        await _publish_as(probe, "w2-x", f"wave.{wave_id}.task.tb",
                          _worker_env("tb", wave_id, "w2-x", "completed",
                                      {"result": "ok"}))
        deadline = asyncio.get_running_loop().time() + 15
        while asyncio.get_running_loop().time() < deadline:
            snap = await hub_queries._wave_status({"wave_id": wave_id})
            if snap["data"]["wave"]["status"] == "completed":
                break
            await asyncio.sleep(0.25)
        assert snap["data"]["wave"]["status"] == "completed", snap
        tasks = {t["task_id"]: t for t in snap["data"]["tasks"]}
        assert tasks["ta"]["verify_result"] == "passed"
        assert snap["data"]["summary"]["merge_gate"] == "completed"
        await probe.close()

        # session.set_backend_ctx + session.get round-trip.
        sid = f"sess-{uuid.uuid4().hex[:6]}"
        r = await conn.api_request("session.create", {
            "session_id": sid, "orchestrator": "pytest", "worker": w1,
            "status": "active", "created_at": conn.now(),
            "updated_at": conn.now(),
        })
        assert r["ok"], r
        r = await conn.api_request("session.set_backend_ctx", {
            "session_id": sid,
            "backend_ctx": {"claude_session_id": "abc-123"},
        })
        assert r["ok"], r
        r = await conn.api_request("session.get", {"session_id": sid})
        assert r["ok"] and r["data"]["session"]["backend_ctx"] == {
            "claude_session_id": "abc-123"}

    asyncio.run(run())
