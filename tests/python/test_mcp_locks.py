"""Tests for the file-lock MCP tools (W2-D).

Unit-level handler checks always run (api_request stubbed). Live tests run
under scripts/dev/with_stack.sh: real hub-server with the lock service,
real channel.lock.<hash> events.
"""
from __future__ import annotations

import asyncio
import json
import os
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "mcp_server"))

import hub_connection as conn  # noqa: E402
import hub_handlers  # noqa: E402
import hub_locks  # noqa: E402


# ── Unit: handlers map to the right hub.api ops ────────────────


def test_lock_files_calls_acquire(monkeypatch):
    calls = []

    async def fake(op, params):
        calls.append((op, params))
        return {"ok": True, "data": {"granted": True,
                                     "lock": {"lock_id": "L1", "holder": params["holder"]},
                                     "channel": "lock.abc"}}

    monkeypatch.setattr(conn, "api_request", fake)
    monkeypatch.setenv("NATS_HUB_IDENTITY", "orch-1")
    res = asyncio.run(hub_handlers.HANDLERS["lock_files"](
        {"scope": "repo", "paths": ["src/a.py", "docs/"], "ttl_secs": 120,
         "reason": "refactor"}))
    assert res["ok"], res
    op, params = calls[0]
    assert op == "lock.acquire"
    assert params["scope"] == "repo" and params["paths"] == ["src/a.py", "docs/"]
    # The holder is always the bound identity, never an argument.
    assert params["holder"] == "orch-1"
    assert params["ttl_secs"] == 120


def test_lock_files_surfaces_conflict(monkeypatch):
    async def fake(op, params):
        return {"ok": True, "data": {
            "granted": False,
            "conflicts": [{"lock_id": "L9", "holder": "other",
                           "paths": ["src/"]}]}}

    monkeypatch.setattr(conn, "api_request", fake)
    monkeypatch.setenv("NATS_HUB_IDENTITY", "orch-1")
    res = asyncio.run(hub_handlers.HANDLERS["lock_files"](
        {"scope": "repo", "paths": ["src/x.py"]}))
    assert res["ok"] is False
    assert res["data"]["conflicts"][0]["holder"] == "other"


def test_release_and_list_passthrough(monkeypatch):
    calls = []

    async def fake(op, params):
        calls.append((op, params))
        return {"ok": True, "data": {"released": params.get("lock_id"),
                                     "locks": [], "channel": "lock.x"}}

    monkeypatch.setattr(conn, "api_request", fake)
    asyncio.run(hub_handlers.HANDLERS["release_files"]({"lock_id": "L1"}))
    asyncio.run(hub_handlers.HANDLERS["release_files"]({"lock_id": "L2", "force": True}))
    asyncio.run(hub_handlers.HANDLERS["list_locks"]({"scope": "repo"}))
    assert calls == [("lock.release", {"lock_id": "L1"}),
                     ("lock.release", {"lock_id": "L2", "force": True}),
                     ("lock.list", {"scope": "repo"})]


def test_paths_overlap_rules():
    o = hub_locks._paths_overlap
    assert o("a/", "a/b.py") and o("a/b.py", "a/") and o("a", "a/")
    assert not o("a/b.py", "a/c.py") and not o("ab.py", "a/")


# ── Live: real hub-server lock service ─────────────────────────

live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh (nats-server + hub-server)",
)


@live
def test_live_acquire_wait_release():
    """A locks, B waits (with event-driven unblock), A releases, B proceeds."""
    async def run():
        os.environ["NATS_HUB_IDENTITY"] = "pytest-a"
        got = await hub_handlers.HANDLERS["lock_files"](
            {"scope": "live-repo", "paths": ["src/main.py"]})
        assert got["ok"], got
        lock_id = got["data"]["lock"]["lock_id"]

        # Waiter B sees A's lock, then unblocks on release.
        async def wait_free():
            os.environ["NATS_HUB_IDENTITY"] = "pytest-b"
            return await hub_locks._wait_for_files(
                {"scope": "live-repo", "paths": ["src/main.py"], "timeout": 20})

        waiter = asyncio.create_task(wait_free())
        await asyncio.sleep(0.5)
        os.environ["NATS_HUB_IDENTITY"] = "pytest-a"
        rel = await hub_handlers.HANDLERS["release_files"]({"lock_id": lock_id})
        assert rel["ok"], rel
        res = await asyncio.wait_for(waiter, timeout=25)
        assert res["ok"] and res["data"]["free"], res

    asyncio.run(run())
