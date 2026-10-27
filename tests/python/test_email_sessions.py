"""email_plugin: one hub session per email thread (D3) and the per-sender
reply mode (D7), against the fake Resend + fake hub of test_email_bridge.py,
plus the session wire shapes HubLink puts on NATS."""
from __future__ import annotations

import json
from types import SimpleNamespace

import pytest

from email_plugin.bridge import EmailBridge
from email_plugin.hub import HubLink
from email_plugin.state import State
from test_email_bridge import FakeHub, make, poll

HOUR = 3600.0


async def first_mail(bridge, api, text="first question", thread="t1", frm="ada@example.org"):
    await poll(bridge)  # baseline (empty inbox)
    api.deliver("ib", thread, frm, text)
    await poll(bridge)


def session_of(bridge, thread="t1"):
    return bridge.state.thread_session("ib", thread)


@pytest.mark.asyncio
async def test_new_thread_starts_a_session_and_follow_ups_reuse_it(tmp_path):
    bridge, api = make(tmp_path)
    await first_mail(bridge, api)
    hub = bridge.hub
    ((op, sid),) = hub.ops
    assert op == "start" and "first question" in hub.calls[0][1]
    assert session_of(bridge)["session_id"] == sid and session_of(bridge)["turns"] == 1
    assert len(api.replies) == 1

    api.deliver("ib", "t1", "ada@example.org", "second question")
    await poll(bridge)
    assert hub.ops[-1] == ("send", sid)
    follow_up = hub.calls[-1][1]
    # The session already has the first email in memory: only what's new.
    assert "second question" in follow_up and "first question" not in follow_up
    assert "untrusted external content" in follow_up
    assert len(api.replies) == 2 and session_of(bridge)["turns"] == 2

    # Another thread gets its own session.
    api.deliver("ib", "t2", "ada@example.org", "unrelated")
    await poll(bridge)
    assert hub.ops[-1][0] == "start" and hub.ops[-1][1] != sid
    assert session_of(bridge, "t2")["session_id"] == hub.ops[-1][1]


@pytest.mark.asyncio
async def test_thread_session_survives_a_plugin_restart(tmp_path):
    bridge, api = make(tmp_path)
    await first_mail(bridge, api)
    sid = session_of(bridge)["session_id"]
    hub = bridge.hub  # the worker (and its session) keeps running
    again = EmailBridge(bridge.cfg, api, hub, State(bridge.cfg.state_path))
    api.deliver("ib", "t1", "ada@example.org", "after restart")
    await poll(again)
    assert hub.ops[-1] == ("send", sid)


@pytest.mark.asyncio
async def test_dead_session_is_replaced_with_thread_context(tmp_path):
    bridge, api = make(tmp_path)
    await first_mail(bridge, api)
    hub = bridge.hub
    old = session_of(bridge)["session_id"]
    hub.held.clear()  # the worker restarted and could not resume the session
    api.deliver("ib", "t1", "ada@example.org", "are you there?")
    await poll(bridge)
    assert [op for op, _ in hub.ops] == ["start", "send", "close", "start"]
    assert hub.ops[1] == ("send", old) and hub.ops[2] == ("close", old)
    new = session_of(bridge)["session_id"]
    assert new != old and hub.ops[3] == ("start", new)
    fresh = hub.calls[-1][1]  # a fresh session gets the whole thread again
    assert "first question" in fresh and "are you there?" in fresh
    assert len(api.replies) == 2  # one reply per email, the failed send isn't one
    assert "acs-failed" not in api.labels["t1"]


@pytest.mark.asyncio
async def test_failed_first_turn_and_timeouts_start_fresh(tmp_path):
    hub = FakeHub(status="error")
    bridge, api = make(tmp_path, hub=hub)
    await first_mail(bridge, api)
    assert "acs-failed" in api.labels["t1"] and session_of(bridge) is None
    assert [op for op, _ in hub.ops] == ["start", "close"]

    hub.status = "done"
    api.deliver("ib", "t1", "ada@example.org", "try again")
    await poll(bridge)
    assert hub.ops[-1][0] == "start" and "first question" in hub.calls[-1][1]
    sid = session_of(bridge)["session_id"]

    hub.status = "timeout"  # a worker that stops answering: forget its session
    api.deliver("ib", "t1", "ada@example.org", "third")
    await poll(bridge)
    assert hub.ops[-2:] == [("send", sid), ("close", sid)] and session_of(bridge) is None


@pytest.mark.asyncio
async def test_idle_sessions_are_closed_and_the_next_email_starts_fresh(tmp_path):
    now = [1000.0]
    bridge, api = make(tmp_path, clock=lambda: now[0], session_idle_hours=2)
    await first_mail(bridge, api)
    hub = bridge.hub
    sid = session_of(bridge)["session_id"]
    now[0] += 1.5 * HOUR
    await poll(bridge)
    assert ("close", sid) not in hub.ops
    now[0] += 1 * HOUR
    await poll(bridge)  # the sweep at the start of each poll
    assert hub.ops[-1] == ("close", sid) and session_of(bridge) is None
    api.deliver("ib", "t1", "ada@example.org", "back again")
    await poll(bridge)
    assert hub.ops[-1][0] == "start" and "first question" in hub.calls[-1][1]


@pytest.mark.asyncio
async def test_oneshot_conversation_never_opens_sessions(tmp_path):
    bridge, api = make(tmp_path, conversation="oneshot")
    await first_mail(bridge, api)
    api.deliver("ib", "t1", "ada@example.org", "second question")
    await poll(bridge)
    assert [op for op, _ in bridge.hub.ops] == ["delegate", "delegate"]
    assert "first question" in bridge.hub.calls[-1][1]  # thread text as context
    assert session_of(bridge) is None and len(api.replies) == 2


@pytest.mark.asyncio
async def test_per_sender_replies_send_to_allowlisted_and_draft_for_others(tmp_path):
    bridge, api = make(tmp_path, allow_from=("*",), auto_send_from=("ada@example.org",))
    await first_mail(bridge, api)
    api.deliver("ib", "t2", "bob@example.net", "hi from a stranger")
    await poll(bridge)
    assert [r[1] for r in api.replies] == ["t1"] and [d[1] for d in api.drafts] == ["t2"]
    assert "acs-replied" in api.labels["t1"] and "acs-drafted" in api.labels["t2"]
    recs = bridge.state.data["processed"]
    assert {r["reply_mode"] for r in recs.values() if "reply_mode" in r} == {"send", "draft"}

    # A bare "*" in allow_from trusts nobody for auto-send.
    bridge, api = make(tmp_path / "b", allow_from=("*",))
    await first_mail(bridge, api)
    assert api.replies == [] and len(api.drafts) == 1

    # Unauthenticated (require_auth off): draft even when allowlisted.
    bridge, api = make(tmp_path / "c", require_auth=False)
    await poll(bridge)
    api.deliver("ib", "t1", "ada@example.org", "hi", auth={"dkim": "fail", "dmarc": "fail"})
    await poll(bridge)
    assert api.replies == [] and len(api.drafts) == 1

    # Explicit send overrides per_sender.
    bridge, api = make(tmp_path / "d", allow_from=("*",), reply_mode="send")
    await first_mail(bridge, api, frm="bob@example.net")
    assert len(api.replies) == 1 and api.drafts == []


# ── HubLink wire shapes ───────────────────────────────────────────────────


class FakeNC:
    """Records publishes/requests; ``answer(env)`` plays the worker: its
    payload (or None for silence) comes back on the envelope's channel."""

    def __init__(self, answer):
        self.answer = answer
        self.published: list[tuple[str, dict]] = []
        self.requests: list[tuple[str, dict]] = []
        self.subs: dict[str, object] = {}

    async def subscribe(self, subject, cb):
        self.subs[subject] = cb
        return SimpleNamespace(unsubscribe=lambda: self._unsub(subject))

    async def _unsub(self, subject):
        self.subs.pop(subject, None)

    async def flush(self):
        pass

    async def publish(self, subject, data):
        env = json.loads(data)
        self.published.append((subject, env))
        payload = self.answer(env)
        cb = self.subs.get(f"channel.{env['meta']['channel']}")
        if payload is not None and cb is not None:
            res = {"meta": {"id": "r1", "from": env["meta"]["to"], "kind": "message",
                            "channel": env["meta"]["channel"], "reply_to": env["meta"]["id"]},
                   "payload": payload}
            await cb(SimpleNamespace(data=json.dumps(res).encode()))

    async def request(self, subject, data, timeout):
        self.requests.append((subject, json.loads(data)))
        return SimpleNamespace(data=b'{"ok": true, "data": {}}')


def worker(env):
    p = env["payload"]
    if env["meta"]["kind"] == "control" or p.get("action") == "session_close":
        return None
    if p.get("action") == "session_send" and p["session_id"] == "gone":
        return {"status": "error", "result": None, "error": "unknown session gone on worker claude"}
    return {"status": "done", "result": "ok"}


@pytest.mark.asyncio
async def test_hublink_session_wire_shapes():
    nc = FakeNC(worker)
    hub = HubLink(nc, "email-bridge")
    out = await hub.start_session("claude", "s1", "hello", 5, metadata={"thread_id": "t1"})
    assert (out.status, out.result, out.session_id) == ("done", "ok", "s1")
    (subject, env), = nc.published
    assert subject == "hub.pub.email-bridge.session.s1"
    assert env["meta"]["to"] == "claude" and env["meta"]["channel"] == "session.s1"
    assert env["payload"] == {"action": "session_start", "session_id": "s1",
                              "session_channel": "session.s1", "prompt": "hello"}
    (api_subject, req), = nc.requests  # the record exists before the turn runs
    assert api_subject == "hub.api.email-bridge.session.create"
    assert req["params"]["orchestrator"] == "email-bridge" and req["params"]["worker"] == "claude"
    assert req["params"]["metadata"]["thread_id"] == "t1"

    out = await hub.send_session("claude", "s1", "more", 5)
    env = nc.published[-1][1]
    assert out.status == "done" and not out.session_gone
    assert env["meta"]["to"] == "claude"  # a DM: reaches a restarted worker's inbox
    assert env["payload"] == {"action": "session_send", "session_id": "s1", "message": "more"}
    assert (await hub.send_session("claude", "gone", "x", 5)).session_gone

    await hub.close_session("claude", "s1")
    assert nc.published[-1][1]["payload"] == {"action": "session_close", "session_id": "s1"}
    assert nc.requests[-1][1] == {"op": "session.update_status",
                                  "params": {"session_id": "s1", "status": "closed"}}

    silent = HubLink(FakeNC(lambda env: None), "email-bridge")
    out = await silent.send_session("claude", "s1", "hello?", 0.1)
    cancel = silent.nc.published[-1][1]
    assert out.status == "timeout" and cancel["meta"]["kind"] == "control"
    assert cancel["payload"] == {"action": "cancel", "task_id": out.task_id}
