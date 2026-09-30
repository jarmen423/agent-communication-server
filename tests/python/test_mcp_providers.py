"""check_providers — the provider health check (TODO.md).

Unit tests use the in-memory bus (test_mcp_fakes). The live test runs under
scripts/dev/with_stack.sh: a real echo worker (ok) and a registered-but-silent
worker (unresponsive) through the real router + agent registry.
"""
from __future__ import annotations

import asyncio
import json
import os
import subprocess
import sys
import time
import uuid

import pytest

from test_mcp_fakes import REPO_ROOT, FakeWorker, agent_record, install

import hub_buffers  # noqa: E402
import hub_connection as conn  # noqa: E402
import hub_providers  # noqa: E402
import nats_hub_mcp  # noqa: E402

H = nats_hub_mcp.HANDLERS


def _registry(bus, agents: list[dict]) -> None:
    def find(params):
        caps = set(params.get("capabilities") or [])
        return {"agents": [a for a in agents
                           if caps <= set(a["capabilities"])]}

    bus.api["agent.find"] = find


def _by_id(data: dict) -> dict:
    return {w["identity"]: w for w in data["workers"]}


def test_lists_alive_workers_with_models_and_sources(monkeypatch):
    async def run():
        bus = install(monkeypatch)
        _registry(bus, [
            agent_record("meta-w", ["worker"], {"provider": "claude",
                                                "model": "opus"}),
            agent_record("sup-w", ["worker"]),
            agent_record("cap-w", ["worker", "provider:codex", "model:gpt-5"]),
            agent_record("bare-w", ["worker", "execute"]),
        ])
        bus.requests["hub.worker.list"] = lambda _b: {"ok": True, "workers": [
            {"identity": "sup-w", "provider": "hermes", "model": "m1"}]}
        bus.requests["hub.worker.models"] = lambda b: {
            "ok": True, "models": [f"{b['provider']}-a"]}

        r = await H["check_providers"]({"alive_within_secs": 60,
                                        "providers": ["claude"]})
        assert r["ok"], r
        d = r["data"]
        assert ("agent.find", {"capabilities": [],
                               "alive_within_secs": 60}) in bus.api_calls
        w = _by_id(d)
        assert w["meta-w"]["models"] == ["opus"]
        assert w["meta-w"]["model_source"] == "registry-metadata"
        assert (w["sup-w"]["provider"], w["sup-w"]["models"],
                w["sup-w"]["model_source"]) == ("hermes", ["m1"], "supervisor")
        assert (w["cap-w"]["provider"], w["cap-w"]["models"],
                w["cap-w"]["model_source"]) == ("codex", ["gpt-5"], "capabilities")
        assert w["bare-w"]["model_source"] is None
        assert all(x["alive"] and x["age_secs"] is not None for x in w.values())
        assert "ping" not in w["bare-w"]  # no ping unless asked
        assert d["supervisor"] == {"reachable": True, "error": None}
        assert d["provider_probes"] == [
            {"provider": "claude", "ok": True, "models": ["claude-a"],
             "error": None}]
        assert any("credits" in s for s in d["does_not_verify"])
        assert d["summary"] == {"alive": 4}

    asyncio.run(run())


def test_ping_classifies_ok_slow_error_unresponsive(monkeypatch):
    async def run():
        bus = install(monkeypatch)
        _registry(bus, [
            agent_record("fast", ["worker"]),
            agent_record("slowpoke", ["worker"]),
            agent_record("broke", ["worker"]),
            agent_record("ghost", ["worker"]),
            agent_record("bridge", ["telegram"]),   # not a worker: not pinged
            agent_record("t-orch", ["worker"]),     # self: not pinged
        ])
        await FakeWorker(bus, "fast").start()
        await FakeWorker(bus, "slowpoke", delay=0.3).start()
        await FakeWorker(bus, "broke", mode="error").start()
        ghost = await FakeWorker(bus, "ghost", mode="silent").start()

        t0 = time.monotonic()
        r = await H["check_providers"]({"ping": True, "ping_timeout": 0.6,
                                        "slow_after": 0.15})
        elapsed = time.monotonic() - t0
        assert r["ok"], r
        w = _by_id(r["data"])
        assert w["fast"]["ping"]["status"] == "ok"
        assert w["fast"]["ping"]["reply"] == "pong"
        assert isinstance(w["fast"]["ping"]["latency_ms"], int)
        assert w["slowpoke"]["ping"]["status"] == "slow"
        assert w["broke"]["ping"]["status"] == "error"
        assert "out of credits" in w["broke"]["ping"]["detail"]
        assert w["ghost"]["ping"]["status"] == "unresponsive"
        assert "cancel sent" in w["ghost"]["ping"]["detail"]
        assert w["bridge"]["ping"] == {"status": "not_pinged"}
        assert w["t-orch"]["ping"] == {"status": "not_pinged"}
        assert r["data"]["summary"] == {"alive": 6, "ok": 1, "slow": 1,
                                        "error": 1, "unresponsive": 1,
                                        "not_pinged": 2}
        # Bounded: pings run in parallel, ~ping_timeout total, never hangs.
        assert elapsed < 2.0, elapsed
        # The timed-out ping was cancelled per §4.2.
        await asyncio.sleep(0.01)
        assert len(ghost.control_seen) == 1
        assert ghost.control_seen[0]["payload"]["action"] == "cancel"
        # Pings leave no tracked tasks behind.
        assert hub_buffers.hub().tasks == {}
        # Each ping is the tiny contract prompt.
        prompts = [e["payload"].get("prompt") for e in bus.published
                   if e["meta"]["kind"] == "message" and e["meta"].get("to")]
        assert set(prompts) == {hub_providers.PING_PROMPT}

    asyncio.run(run())


def test_ping_explicit_workers_includes_unregistered(monkeypatch):
    async def run():
        bus = install(monkeypatch)
        _registry(bus, [agent_record("a", ["worker"]),
                        agent_record("b", ["worker"])])
        await FakeWorker(bus, "a").start()
        r = await H["check_providers"]({"ping": True, "workers": ["a", "gone"],
                                        "ping_timeout": 0.2})
        w = _by_id(r["data"])
        assert w["a"]["ping"]["status"] == "ok"
        assert w["gone"]["alive"] is False
        assert w["gone"]["ping"]["status"] == "unresponsive"
        assert w["b"]["ping"] == {"status": "not_pinged"}

    asyncio.run(run())


def test_ping_target_cap(monkeypatch):
    monkeypatch.setattr(hub_providers, "MAX_PING_TARGETS", 2)

    async def run():
        bus = install(monkeypatch)
        _registry(bus, [agent_record(f"w{i}", ["worker"]) for i in range(4)])
        for i in range(4):
            await FakeWorker(bus, f"w{i}").start()
        r = await H["check_providers"]({"ping": True, "ping_timeout": 1})
        d = r["data"]
        assert d["summary"]["ok"] == 2 and d["summary"]["not_pinged"] == 2
        assert "first 2 of 4" in d["notes"][0]

    asyncio.run(run())


def test_supervisor_absent_and_hub_down(monkeypatch):
    async def run():
        bus = install(monkeypatch)
        _registry(bus, [])
        r = await H["check_providers"]({"providers": ["claude"]})
        assert r["ok"]
        assert r["data"]["supervisor"]["reachable"] is False
        assert "no responders" in r["data"]["supervisor"]["error"]
        assert r["data"]["provider_probes"][0]["ok"] is False

        del bus.api["agent.find"]  # hub-server not running
        r = await H["check_providers"]({})
        assert not r["ok"]
        assert "hub-server is not answering" in r["error"]

    asyncio.run(run())


def test_check_providers_argument_validation(monkeypatch):
    async def run():
        install(monkeypatch)
        for bad, frag in (({"ping": "yes"}, "is not of type 'boolean'"),
                          ({"ping_timeout": 0}, "ping_timeout"),
                          ({"ping_timeout": 999}, "ping_timeout"),
                          ({"alive_within_secs": 0}, "alive_within_secs"),
                          ({"workers": "a"}, "is not of type 'array'"),
                          ({"worker": ["a"]}, "'worker' was unexpected")):
            r = await H["check_providers"](bad)
            assert not r["ok"] and frag in r["error"], (bad, r)

    asyncio.run(run())


# ── Live (with_stack.sh only) ─────────────────────────────────────

live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh (nats-server + hub-server)",
)


@live
def test_check_providers_live(monkeypatch):
    """Real router + registry: an echo worker pings ok; a worker that
    registers but never answers is reported unresponsive within the bound."""
    monkeypatch.setenv("NATS_HUB_IDENTITY", f"pytest-orch-{uuid.uuid4().hex[:6]}")
    echo_id = f"echo-{uuid.uuid4().hex[:6]}"
    ghost_id = f"ghost-{uuid.uuid4().hex[:6]}"

    async def run():
        import nats

        worker = subprocess.Popen(
            [sys.executable, "echo_worker.py", "--identity", echo_id,
             "--nats-url", os.environ["NATS_URL"]],
            cwd=REPO_ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        gnc = await nats.connect(os.environ["NATS_URL"])
        reg = {"meta": {"id": str(uuid.uuid4()), "from": ghost_id,
                        "channel": "system", "to": None, "kind": "control",
                        "timestamp": conn.now(), "reply_to": None},
               "payload": {"identity": ghost_id, "capabilities": ["worker"]}}
        await gnc.publish("hub.register", json.dumps(reg).encode())
        await gnc.subscribe(f"channel.inbox.{ghost_id}")  # receives, never answers
        await gnc.flush()
        try:
            deadline = time.monotonic() + 30
            while True:  # registry mirror is async: wait for both agents
                r = await H["list_agents"]({"alive_within_secs": 60})
                assert r["ok"], r
                ids = {a["identity"] for a in r["data"]["agents"]}
                if {echo_id, ghost_id} <= ids:
                    break
                assert time.monotonic() < deadline, f"not registered: {ids}"
                await asyncio.sleep(0.3)

            t0 = time.monotonic()
            r = await H["check_providers"]({
                "ping": True, "workers": [echo_id, ghost_id],
                "ping_timeout": 8, "alive_within_secs": 60})
            elapsed = time.monotonic() - t0
            assert r["ok"], r
            w = _by_id(r["data"])
            assert w[echo_id]["alive"] and w[echo_id]["ping"]["status"] in ("ok", "slow")
            assert w[echo_id]["ping"]["reply"].startswith("echo:")
            assert w[ghost_id]["ping"]["status"] == "unresponsive"
            assert elapsed < 8 + 5, elapsed
        finally:
            worker.terminate()
            worker.wait(timeout=5)
            await gnc.close()
            await conn.close()
            hub_buffers._state = None

    asyncio.run(run())


def test_age_secs_parses_chrono_timestamps():
    assert hub_providers._age_secs("2026-01-01T00:00:00.123456789Z") > 0
    assert hub_providers._age_secs("2026-01-01T00:00:00+00:00") > 0
    assert hub_providers._age_secs("not a time") is None
    assert hub_providers._age_secs(None) is None
