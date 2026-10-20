"""Human bridges (hardening H1 #6): Telegram/Discord publish human → agent
DMs on the bound subject with ``meta.to`` set."""

import asyncio
import json
import os
import uuid

import pytest

import discord_bridge
import telegram_bridge

live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh",
)


@pytest.mark.parametrize("mod", [telegram_bridge, discord_bridge])
def test_dm_publish_uses_bound_subject_and_sets_to(mod):
    subject, body = mod.dm_publish("human-bridge-x", "orchestrator", "hello")
    assert subject == "hub.pub.human-bridge-x.inbox.orchestrator"
    env = json.loads(body)
    assert env["meta"]["to"] == "orchestrator"
    assert env["meta"]["from"] == "human-bridge-x"
    assert env["meta"]["channel"] == "inbox.orchestrator"
    assert env["payload"] == {"message": "hello"}


@pytest.mark.parametrize("mod", [telegram_bridge, discord_bridge])
def test_no_legacy_subjects_in_bridges(mod):
    with open(mod.__file__) as f:
        src = f.read()
    assert "hub.send." not in src, f"{mod.__name__} still publishes on legacy hub.send"


@live
@pytest.mark.parametrize("mod", [telegram_bridge, discord_bridge])
def test_bridge_dm_arrives_from_the_bridge(mod):
    import nats

    async def run():
        url = os.environ["NATS_URL"]
        bridge = f"bridge-{uuid.uuid4().hex[:6]}"
        rcpt = f"rcpt-{uuid.uuid4().hex[:6]}"
        watcher = await nats.connect(url)
        sender = await nats.connect(url)
        try:
            sub = await watcher.subscribe(f"channel.inbox.{rcpt}")
            await watcher.flush()
            subject, body = mod.dm_publish(bridge, rcpt, "from a human")
            await sender.publish(subject, body)
            await sender.flush()
            msg = await sub.next_msg(timeout=5)
            return bridge, rcpt, json.loads(msg.data)
        finally:
            await sender.close()
            await watcher.close()

    bridge, rcpt, env = asyncio.run(run())
    assert env["meta"]["from"] == bridge
    assert env["meta"]["to"] == rcpt
    assert env["payload"]["message"] == "from a human"
