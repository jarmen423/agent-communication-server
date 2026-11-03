"""`meta.origin` as Python consumers see it: a plain dict, stamped by the router
on an envelope published with the worker runtime's own helper."""

import asyncio
import json
import os
import uuid

import pytest

from worker_backends.envelope import make_envelope

live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh",
)


async def _round_trip(identity: str, channel: str, to: str | None) -> dict:
    import nats

    nc = await nats.connect(os.environ["NATS_URL"])
    try:
        sub = await nc.subscribe(f"channel.inbox.{to}" if to else f"channel.{channel}")
        await nc.flush()
        await asyncio.sleep(0.2)
        body = make_envelope(identity, to, channel, "message", {"t": 1})
        await nc.publish(f"hub.pub.{identity}.{channel}", body)
        await nc.flush()
        msg = await sub.next_msg(timeout=10)
        return json.loads(msg.data)
    finally:
        await nc.close()


@live
@pytest.mark.parametrize("to", [None, f"origin-rcpt-{uuid.uuid4().hex[:8]}"])
def test_router_stamps_origin_as_a_dict(to):
    identity = f"origin-{uuid.uuid4().hex[:8]}"
    channel = f"origin-{uuid.uuid4().hex[:8]}"
    env = asyncio.run(_round_trip(identity, channel, to))
    origin = (env.get("meta") or {}).get("origin") or {}
    assert isinstance(origin, dict)
    assert origin.get("identity") == identity
    assert origin.get("tenant") == "test-tenant"
    # This identity has no manifest record, so the hub omits the owner fields.
    assert "user" not in origin
    assert "reported" not in origin
