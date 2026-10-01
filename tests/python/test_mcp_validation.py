"""Every MCP tool: schema-validated arguments, a smoke call, actionable errors.

``SAMPLE_ARGS`` must name every tool — adding a tool without adding it here
fails ``test_every_tool_has_sample_args``, so each tool gets at least:
a valid call against the in-memory bus, a missing-required-arg rejection and
an unknown-arg rejection.
"""
from __future__ import annotations

import asyncio
import importlib.util

import pytest

from test_mcp_fakes import REPO_ROOT, install

import hub_connection as conn  # noqa: E402
import hub_validate  # noqa: E402
import nats_hub_mcp  # noqa: E402

H = nats_hub_mcp.HANDLERS
TOOLS = {t.name: t for t in nats_hub_mcp.TOOLS}

SAMPLE_ARGS: dict[str, dict] = {
    "whoami": {},
    "list_agents": {"alive_within_secs": 60, "limit": 5},
    "get_agent": {"identity": "w1"},
    "check_providers": {"alive_within_secs": 30},
    "send_message": {"channel": "general", "message": "hi"},
    "send_direct": {"to": "w1", "message": "hi"},
    "send_status": {"channel": "general", "status": "idle"},
    "read_inbox": {"limit": 5},
    "wait_for_message": {"timeout": 0.01},
    "delegate_async": {"to": "w1", "prompt": "p"},
    "delegate_task": {"to": "w1", "prompt": "p", "timeout": 0.05},
    "task_status": {"task_id": "unknown"},
    "wait_for_task": {"task_id": "unknown", "timeout": 0.01},
    "cancel_task": {"task_id": "unknown"},
    "start_session": {"worker": "w1", "prompt": "p"},
    "send_to_session": {"session_id": "s1", "message": "m"},
    "close_session": {"session_id": "s1"},
    "session_replies": {"session_id": "s1"},
    "list_sessions": {"status": "active"},
    "get_session": {"session_id": "s1"},
    "get_history": {"channel": "general", "limit": 5},
    "get_thread": {"root_id": "r1"},
    "list_pending": {"identity": "w1"},
    "create_wave": {"goal": "g", "tasks": [{"worker": "w1", "goal": "t"}]},
    "spawn_wave": {"wave_id": "wv1", "timeout": 1},
    "list_waves": {},
    "get_wave": {"wave_id": "wv1"},
    "list_wave_tasks": {"wave_id": "wv1"},
    "get_wave_task": {"wave_id": "wv1", "task_id": "t1"},
    "wave_status": {"wave_id": "wv1"},
    "cancel_wave": {"wave_id": "wv1"},
    "get_analytics": {"metric": "latency", "secs": 60},
}


def _bus(monkeypatch):
    bus = install(monkeypatch)

    class _AnyOp(dict):  # every hub.api op answers {} (wave.list_tasks: none)
        def get(self, op, default=None):
            return super().get(op, lambda _p: {})

    bus.api = _AnyOp()
    return bus


def test_every_tool_has_sample_args():
    assert set(SAMPLE_ARGS) == set(TOOLS) == set(H)


def test_schemas_are_strict_objects():
    for name, tool in TOOLS.items():
        schema = tool.inputSchema
        assert schema["type"] == "object", name
        assert schema.get("additionalProperties") is False, name
        for req in schema.get("required", []):
            assert req in schema["properties"], (name, req)


@pytest.mark.parametrize("name", sorted(SAMPLE_ARGS))
def test_tool_valid_call_returns_well_formed_result(monkeypatch, name):
    async def run():
        _bus(monkeypatch)
        res = await asyncio.wait_for(H[name](dict(SAMPLE_ARGS[name])), 10)
        assert isinstance(res, dict) and isinstance(res.get("ok", True), bool)
        if res.get("ok") is False:
            assert isinstance(res["error"], str) and res["error"], res
            assert "invalid arguments" not in res["error"], res

    asyncio.run(run())


@pytest.mark.parametrize("name", sorted(SAMPLE_ARGS))
def test_tool_rejects_unknown_and_missing_args(monkeypatch, name):
    async def run():
        _bus(monkeypatch)
        res = await H[name]({**SAMPLE_ARGS[name], "bogus_arg": 1})
        assert not res["ok"] and "'bogus_arg' was unexpected" in res["error"]
        assert res["error"].startswith(f"{name}: invalid arguments")
        assert "Expected:" in res["error"]
        for req in TOOLS[name].inputSchema.get("required", []):
            args = {k: v for k, v in SAMPLE_ARGS[name].items() if k != req}
            res = await H[name](args)
            assert not res["ok"]
            assert f"'{req}' is a required property" in res["error"]
        res = await H[name]("not-an-object")
        assert not res["ok"] and "must be a JSON object" in res["error"]

    asyncio.run(run())


def test_type_errors_name_the_field(monkeypatch):
    async def run():
        _bus(monkeypatch)
        r = await H["wait_for_task"]({"task_id": 5, "timeout": 1})
        assert "is not of type 'string'" in r["error"] and "'task_id'" in r["error"]
        r = await H["read_inbox"]({"limit": 0})
        assert "limit" in r["error"]

    asyncio.run(run())


def test_legacy_from_arg(monkeypatch):
    async def run():
        _bus(monkeypatch)  # identity t-orch
        r = await H["send_message"]({"channel": "c", "message": "m",
                                     "from": "mallory"})
        assert not r["ok"] and "does not match NATS_HUB_IDENTITY" in r["error"]
        r = await H["send_message"]({"channel": "c", "message": "m",
                                     "from": "t-orch"})
        assert r["ok"], r  # matching legacy arg is tolerated and dropped
        # `from` as a declared *filter* is not an identity claim.
        r = await H["wait_for_message"]({"timeout": 0.01, "from": "anyone"})
        assert "does not match" not in r["error"]

    asyncio.run(run())


def test_identity_unset_is_actionable(monkeypatch):
    async def run():
        _bus(monkeypatch)
        monkeypatch.delenv("NATS_HUB_IDENTITY")
        r = await H["whoami"]({})
        assert not r["ok"] and "NATS_HUB_IDENTITY is not set" in r["error"]
        r = await H["delegate_async"]({"to": "w", "prompt": "p"})
        assert not r["ok"] and "NATS_HUB_IDENTITY" in r["error"]

    asyncio.run(run())


def test_whoami_reports_bound_identity(monkeypatch):
    async def run():
        _bus(monkeypatch)
        r = await H["whoami"]({})
        assert r["ok"] and r["data"]["identity"] == "t-orch"
        assert r["data"]["nats_url"] == conn.NATS_URL

    asyncio.run(run())


def test_connection_errors_get_a_hint(monkeypatch):
    async def run():
        install(monkeypatch)

        async def dead():
            raise ConnectionRefusedError(
                "[Errno 111] Connect call failed ('127.0.0.1', 1)")

        monkeypatch.setattr(conn, "get_nc", dead)
        r = await H["send_message"]({"channel": "c", "message": "m"})
        assert not r["ok"] and "cannot reach NATS" in r["error"]
        assert "make up" in r["error"]

    asyncio.run(run())


def test_hint_rules():
    h = hub_validate._hint
    assert "hub-server is not answering" in h(
        "query API 'agent.find' failed: nats: no responders available for request")
    assert "cannot reach NATS" in h("nats: no servers available for connection")
    assert "credentials" in h("nats: 'Authorization Violation'")
    assert h("plain") == "plain"


def test_validated_handlers_rejects_mismatch():
    with pytest.raises(RuntimeError, match="tool/handler mismatch"):
        hub_validate.validated_handlers(nats_hub_mcp.TOOLS, {"whoami": None})


def test_hermes_actions_match_tools():
    spec = importlib.util.spec_from_file_location(
        "hermes_plugin_actions", REPO_ROOT / "hermes-plugin" / "__init__.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    assert set(mod.ACTIONS) == set(TOOLS)
