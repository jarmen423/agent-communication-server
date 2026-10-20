"""Identity binding (iteration-2 T1): identity validation, bound subject
strings on every Python publisher, and live bound-mode checks."""

import asyncio
import json
import os
import sys
import uuid
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT))

import nats_connect  # noqa: E402


# ── Identity validation ────────────────────────────────────────────


def test_validate_identity_accepts_single_tokens():
    for ok in ["alice", "a-b_c9", "Z", "worker-1_prod_x"]:
        assert nats_connect.validate_identity(ok) == ok


def test_validate_identity_rejects_non_tokens():
    for bad in ["", "a.b", "a b", "a*b", "a>b"]:
        with pytest.raises(ValueError):
            nats_connect.validate_identity(bad)


# ── Bound subject strings (no live NATS needed) ────────────────────


def _fake_nc(record: list, reply: dict | None = None):
    class _Fake:
        is_closed = False

        async def publish(self, subject: str, data: bytes):
            record.append(subject)

        async def flush(self):
            pass

        async def request(self, subject: str, data: bytes, timeout=None):
            record.append(subject)

            class _Msg:
                def __init__(self, d):
                    self.data = d

            return _Msg(json.dumps(reply or {"ok": True, "data": {}}).encode())

    return _Fake()


def test_hub_connection_publish_uses_bound_subject(monkeypatch):
    """mcp_server/hub_connection.publish -> hub.pub.<identity>.<channel>."""
    monkeypatch.setenv("NATS_HUB_IDENTITY", "mcp-orch")
    from mcp_server import hub_connection

    record = []

    async def fake_get_nc():
        return _fake_nc(record)

    monkeypatch.setattr(hub_connection, "get_nc", fake_get_nc)

    asyncio.run(hub_connection.publish("chat", {"meta": {}, "payload": {}}))
    assert record == ["hub.pub.mcp-orch.chat"]


def test_hub_connection_api_request_uses_bound_subject(monkeypatch):
    """api_request -> hub.api.<identity>.<op>."""
    monkeypatch.setenv("NATS_HUB_IDENTITY", "mcp-orch")
    from mcp_server import hub_connection

    record = []

    async def fake_get_nc():
        return _fake_nc(record)

    monkeypatch.setattr(hub_connection, "get_nc", fake_get_nc)

    out = asyncio.run(hub_connection.api_request("agent.find", {}))
    assert out["ok"] is True
    assert record == ["hub.api.mcp-orch.agent.find"]


def test_hub_connection_identity_must_be_valid(monkeypatch):
    """identity() fails loudly on an unset or invalid NATS_HUB_IDENTITY."""
    monkeypatch.delenv("NATS_HUB_IDENTITY", raising=False)
    from mcp_server import hub_connection

    with pytest.raises(RuntimeError):
        hub_connection.identity()
    monkeypatch.setenv("NATS_HUB_IDENTITY", "not a token")
    with pytest.raises(RuntimeError):
        hub_connection.identity()


def test_worker_runtime_publishes_bound_subjects():
    """worker_runtime only publishes on the bound subjects (contract §4.1)."""
    src = (REPO_ROOT / "worker_runtime.py").read_text()
    assert 'f"hub.pub.{cfg.identity}.{channel}"' in src
    assert 'f"hub.register.{cfg.identity}"' in src
    assert 'f"hub.presence.{cfg.identity}"' in src
    # no legacy *subject* publish remains (the envelope channel field
    # legitimately stays "hub.presence"/"hub.register" — that is payload)
    for legacy in (
        'publish("hub.register"',
        'publish("hub.presence"',
        'publish(f"hub.send.',
        'publish("hub.send.',
    ):
        assert legacy not in src, legacy


def test_worker_events_bound_subjects():
    src = (REPO_ROOT / "worker_events.py").read_text()
    assert '"hub.send.' not in src
    assert '"hub.presence"' not in src


# ── Live bound-mode checks ─────────────────────────────────────────

live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh",
)

bound_only = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_REQUIRE_BOUND"),
    reason="needs NATS_HUB_REQUIRE_BOUND=1 with_stack.sh mode",
)


def _envelope(frm: str, to: str | None, channel: str, payload: dict) -> bytes:
    return json.dumps({
        "meta": {
            "id": str(uuid.uuid4()),
            "from": frm,
            "to": to,
            "channel": channel,
            "timestamp": "2026-01-01T00:00:00Z",
            "kind": "message",
        },
        "payload": payload,
    }).encode()


@live
def test_forged_from_rewritten_live():
    """Over the wire: meta.from='mallory' on hub.pub.alice.* arrives as alice."""
    import nats

    async def run() -> str:
        url = os.environ["NATS_URL"]
        alice = f"alice-{uuid.uuid4().hex[:6]}"
        bob = f"bob-{uuid.uuid4().hex[:6]}"
        channel = f"idn-{uuid.uuid4().hex[:6]}"
        sender = await nats.connect(url)
        watcher = await nats.connect(url)
        try:
            sub = await watcher.subscribe(f"channel.inbox.{bob}")
            await watcher.flush()
            await asyncio.sleep(0.2)
            await sender.publish(
                f"hub.pub.{alice}.{channel}",
                _envelope("mallory", bob, channel, {"t": 1}),
            )
            await sender.flush()
            msg = await sub.next_msg(timeout=5)
            return alice, json.loads(msg.data)["meta"]["from"]
        finally:
            await sender.close()
            await watcher.close()

    alice, arrived_from = asyncio.run(run())
    assert arrived_from == alice, f"meta.from not rewritten: {arrived_from}"


@bound_only
def test_legacy_subjects_dropped_under_require_bound():
    """Require-bound mode: legacy hub.send.* is dropped; hub.pub.<id>.* routes."""
    import nats

    async def run() -> tuple[bool, bool]:
        url = os.environ["NATS_URL"]
        alice = f"alice-{uuid.uuid4().hex[:6]}"
        bob = f"bob-{uuid.uuid4().hex[:6]}"
        channel = f"idn-{uuid.uuid4().hex[:6]}"
        sender = await nats.connect(url)
        watcher = await nats.connect(url)
        try:
            sub = await watcher.subscribe(f"channel.inbox.{bob}")
            await watcher.flush()
            await asyncio.sleep(0.2)

            await sender.publish(
                f"hub.send.{channel}", _envelope("alice", bob, channel, {"t": "legacy"})
            )
            await sender.flush()
            try:
                await sub.next_msg(timeout=1.5)
                legacy_arrived = True
            except Exception:
                legacy_arrived = False

            await sender.publish(
                f"hub.pub.{alice}.{channel}",
                _envelope("alice", bob, channel, {"t": "bound"}),
            )
            await sender.flush()
            try:
                await sub.next_msg(timeout=5)
                bound_arrived = True
            except Exception:
                bound_arrived = False
            return legacy_arrived, bound_arrived
        finally:
            await sender.close()
            await watcher.close()

    legacy, bound = asyncio.run(run())
    assert legacy is False, "legacy hub.send.* routed under require-bound"
    assert bound is True, "bound subject did not route under require-bound"
