"""Per-identity reply inbox (hardening H1, contract C1): every Python client
connects with ``inbox_prefix=_INBOX.<identity>`` via ``nats_connect``."""

import asyncio
import json
import os
import uuid

import pytest

import nats_connect

live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh",
)


def test_inbox_prefix_precedence(monkeypatch):
    monkeypatch.delenv("NATS_HUB_IDENTITY", raising=False)
    f = nats_connect.inbox_prefix_for
    assert f() is None
    # the NATS user is the permission key, then identity, then name
    assert f(user="alice", identity="w1", name="n") == "_INBOX.alice"
    assert f(user="alice@example.com", identity="w1") == "_INBOX.w1"
    assert f(identity="w1", name="n") == "_INBOX.w1"
    assert f(name="worker-7") == "_INBOX.worker-7"
    assert f(name="not valid") is None
    monkeypatch.setenv("NATS_HUB_IDENTITY", "mcp-orch")
    assert f() == "_INBOX.mcp-orch"
    assert f(name="w2") == "_INBOX.w2"


def test_connect_nats_passes_inbox_prefix(monkeypatch):
    seen = {}

    async def fake_connect(**kwargs):
        seen.update(kwargs)
        return object()

    monkeypatch.delenv("NATS_HUB_IDENTITY", raising=False)
    monkeypatch.delenv("NATS_USER", raising=False)
    monkeypatch.setattr(nats_connect.nats, "connect", fake_connect)
    asyncio.run(nats_connect.connect_nats("nats://x:1", name="w1"))
    assert seen["inbox_prefix"] == "_INBOX.w1"
    seen.clear()
    asyncio.run(nats_connect.connect_nats("nats://x:1", user="bob", password="p",
                                          identity="w1"))
    assert seen["inbox_prefix"] == "_INBOX.bob"
    seen.clear()
    # an explicit inbox_prefix still wins
    asyncio.run(nats_connect.connect_nats("nats://x:1", identity="w1",
                                          inbox_prefix="_INBOX.custom"))
    assert seen["inbox_prefix"] == "_INBOX.custom"


def test_vendored_copies_match():
    """mcp_server ships a vendored nats_connect; it must not drift."""
    root = os.path.dirname(os.path.abspath(nats_connect.__file__))
    with open(os.path.join(root, "nats_connect.py")) as a, \
            open(os.path.join(root, "mcp_server", "nats_connect.py")) as b:
        assert a.read() == b.read(), "run scripts/dev/sync_plugins.sh"


@live
def test_request_reply_lands_in_identity_inbox():
    import nats

    async def run() -> str:
        url = os.environ["NATS_URL"]
        me = f"rr-{uuid.uuid4().hex[:8]}"
        subject = f"test.rr.{uuid.uuid4().hex[:8]}"
        responder = await nats.connect(url)

        async def echo(msg):
            await msg.respond(json.dumps({"reply": msg.reply}).encode())

        await responder.subscribe(subject, cb=echo)
        await responder.flush()
        nc = await nats_connect.connect_nats(url, name=me)
        try:
            resp = await nc.request(subject, b"{}", timeout=5)
            return me, json.loads(resp.data)["reply"]
        finally:
            await nc.close()
            await responder.close()

    me, reply = asyncio.run(run())
    assert reply.startswith(f"_INBOX.{me}."), reply
