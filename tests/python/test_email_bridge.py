"""email_plugin: the poll → gate → wake → reply loop, the Resend client, and a
live round trip through the real router with echo_worker.py."""
from __future__ import annotations

import asyncio
import json
import os
import subprocess
import sys
import uuid

import httpx
import pytest

from email_plugin.bridge import EmailBridge
from email_plugin.config import Mailbox, PluginConfig
from email_plugin.hub import HubLink, TaskOutcome, email_request
from email_plugin.resend_api import ResendError, ResendInboxes
from email_plugin.state import State

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
PASS = {"spf": "pass", "dkim": "pass", "dmarc": "pass"}


class FakeInboxes:
    """In-memory stand-in for ResendInboxes."""

    def __init__(self):
        self.threads: dict[str, dict[str, list[dict]]] = {}  # inbox → thread → emails
        self.received: dict[str, dict] = {}
        self.replies: list[tuple] = []
        self.drafts: list[tuple] = []
        self.sent: list[tuple] = []
        self.labels: dict[str, set[str]] = {}

    def deliver(self, inbox, thread, frm, text, auth=PASS, headers=None, subject="hello"):
        eid = f"e{uuid.uuid4().hex[:6]}"
        self.threads.setdefault(inbox, {}).setdefault(thread, []).append(
            {"id": eid, "direction": "inbound", "from": frm, "to": ["x"], "subject": subject,
             "text": text, "attachments": [], "received_at": "now"})
        self.received[eid] = {"authentication": auth, "headers": headers or {}}
        return eid

    async def list_threads(self, inbox, folders="inbox", limit=50):
        return [{"id": t, "email_count": len(es)} for t, es in self.threads.get(inbox, {}).items()]

    async def list_thread_emails(self, inbox, thread):
        return list(self.threads[inbox][thread])

    async def received_email(self, email_id):
        return self.received[email_id]

    async def reply(self, inbox, thread, email_id, text, headers=None):
        self.replies.append((inbox, thread, email_id, text, headers))
        self.threads[inbox][thread].append({"id": f"r{len(self.replies)}", "direction": "outbound"})
        return f"sent-{email_id}"

    async def draft_reply(self, inbox, thread, email_id, text):
        self.drafts.append((inbox, thread, email_id, text))
        return f"draft-{email_id}"

    async def send_new(self, inbox, to, subject, text, headers=None, idempotency_key=None):
        self.sent.append((inbox, to, subject, text, headers))
        return "new-1"

    async def label_id(self, inbox, name, color):
        return name

    async def add_labels(self, inbox, thread, ids):
        self.labels.setdefault(thread, set()).update(ids)


class FakeHub:
    def __init__(self, result="Sure, done.", status="done"):
        self.calls: list[tuple[str, str]] = []
        self.result, self.status = result, status

    async def delegate(self, worker, prompt, timeout, on_start=None):
        self.calls.append((worker, prompt))
        return TaskOutcome("task-1", self.status, self.result,
                           None if self.status == "done" else "boom")

    async def serve_requests(self, handler):
        pass


def make(tmp_path, hub=None, **mb_kw):
    mb = Mailbox(**{"address": "claude@acs.example.com", "agent": "claude", "inbox_id": "ib",
                    "allow_from": ("ada@example.org",), **mb_kw})
    cfg = PluginConfig(resend_api_key_file="k", state_path=str(tmp_path / "s.json"),
                       mailboxes=(mb,))
    api = FakeInboxes()
    bridge = EmailBridge(cfg, api, hub or FakeHub(), State(cfg.state_path))
    return bridge, api


async def poll(bridge):
    await bridge.poll_once()
    await bridge.drain()


@pytest.mark.asyncio
async def test_existing_mail_is_baselined_not_woken(tmp_path):
    bridge, api = make(tmp_path)
    api.deliver("ib", "t1", "ada@example.org", "old mail")
    await poll(bridge)
    assert bridge.hub.calls == [] and api.replies == []
    api.deliver("ib", "t1", "ada@example.org", "new mail")
    await poll(bridge)
    assert len(bridge.hub.calls) == 1 and "new mail" in bridge.hub.calls[0][1]
    assert api.replies[0][3] == "Sure, done."
    assert api.replies[0][4] == {"Auto-Submitted": "auto-replied"}
    assert {"acs-woke", "acs-replied"} <= api.labels["t1"]


@pytest.mark.asyncio
async def test_each_email_wakes_at_most_once(tmp_path):
    bridge, api = make(tmp_path)
    await poll(bridge)  # baseline (empty)
    api.deliver("ib", "t1", "ada@example.org", "one")
    await poll(bridge)
    await poll(bridge)
    assert len(bridge.hub.calls) == 1
    # A fresh process with the same state file doesn't wake it again either.
    again = EmailBridge(bridge.cfg, api, FakeHub(), State(bridge.cfg.state_path))
    await poll(again)
    assert again.hub.calls == []


@pytest.mark.asyncio
async def test_spam_and_strangers_cost_no_tokens(tmp_path):
    bridge, api = make(tmp_path)
    await poll(bridge)
    api.deliver("ib", "t1", "stranger@spam.example", "buy now")
    api.deliver("ib", "t2", "news@example.org", "weekly", headers={"List-Id": "<n>"})
    api.deliver("ib", "t3", "ada@example.org", "forged", auth={"dkim": "fail", "dmarc": "fail"})
    await poll(bridge)
    assert bridge.hub.calls == []
    assert api.labels == {"t1": {"acs-held"}, "t2": {"acs-ignored"}, "t3": {"acs-held"}}


@pytest.mark.asyncio
async def test_budget_caps_wakes(tmp_path):
    bridge, api = make(tmp_path, max_wakes_per_hour=2)
    await poll(bridge)
    for i in range(4):
        api.deliver("ib", f"t{i}", "ada@example.org", f"m{i}")
    await poll(bridge)
    assert len(bridge.hub.calls) == 2
    assert sum("acs-held" in v for v in api.labels.values()) == 2


@pytest.mark.asyncio
async def test_no_reply_failure_and_draft_modes(tmp_path):
    bridge, api = make(tmp_path, hub=FakeHub(result="NO_REPLY"))
    await poll(bridge)
    api.deliver("ib", "t1", "ada@example.org", "fyi")
    await poll(bridge)
    assert api.replies == []

    bridge, api = make(tmp_path / "b", hub=FakeHub(status="error"))
    await poll(bridge)
    api.deliver("ib", "t1", "ada@example.org", "do it")
    await poll(bridge)
    assert api.replies == [] and "acs-failed" in api.labels["t1"]

    bridge, api = make(tmp_path / "c", reply_mode="draft")
    await poll(bridge)
    api.deliver("ib", "t1", "ada@example.org", "do it")
    await poll(bridge)
    assert api.replies == [] and api.drafts[0][3] == "Sure, done."


@pytest.mark.asyncio
async def test_agent_send_requests_follow_may_email(tmp_path):
    bridge, api = make(tmp_path, may_email=("*@acs.example.com",))
    out = await bridge.send_for_agent("claude", {"to": "bot2@acs.example.com",
                                                 "subject": "hi", "text": "ping"})
    assert "sent new-1" in out and api.sent[0][1] == ["bot2@acs.example.com"]
    assert api.sent[0][4] == {"Auto-Submitted": "auto-generated"}
    with pytest.raises(PermissionError):
        await bridge.send_for_agent("claude", {"to": "eve@evil.example", "subject": "s", "text": "t"})
    with pytest.raises(PermissionError):
        await bridge.send_for_agent("nobody", {"to": "bot2@acs.example.com", "subject": "s", "text": "t"})
    with pytest.raises(ValueError):
        await bridge.send_for_agent("claude", {"to": "bot2@acs.example.com", "subject": "", "text": "t"})


def test_email_request_shapes():
    req = {"to": ["a@b.co"], "subject": "s", "text": "t"}
    assert email_request({"email": req}) == req
    assert email_request({"message": json.dumps({"email": req})}) == req  # MCP send_direct
    assert email_request({"message": "just chatting"}) is None
    assert email_request({"message": "{not json"}) is None
    assert email_request("nope") is None


@pytest.mark.asyncio
async def test_resend_client_paths_retries_and_errors():
    seen: list[tuple[str, str, dict | None]] = []
    flaky = {"n": 0}

    def handler(req: httpx.Request) -> httpx.Response:
        body = json.loads(req.content) if req.content else None
        seen.append((req.method, req.url.path, body))
        assert req.headers["authorization"] == "Bearer re_test"
        if req.url.path.endswith("/threads") and flaky["n"] == 0:
            flaky["n"] += 1
            return httpx.Response(429, headers={"retry-after": "0"})
        if req.url.path.endswith("/labels") and req.method == "GET":
            return httpx.Response(200, json={"data": [{"id": "L1", "name": "acs-woke"}]})
        if req.url.path.endswith("/reply"):
            assert req.headers["idempotency-key"] == "acs-reply-e1"
            return httpx.Response(200, json={"id": "sent1"})
        if req.url.path == "/emails/receiving/bad":
            return httpx.Response(404, json={"message": "not found"})
        return httpx.Response(200, json={"data": [{"id": "t1"}], "id": "x"})

    api = ResendInboxes("re_test", transport=httpx.MockTransport(handler), min_interval=0)
    assert await api.list_threads("ib") == [{"id": "t1"}]
    assert await api.label_id("ib", "ACS-WOKE", "#000") == "L1"
    assert await api.reply("ib", "t1", "e1", "hi", headers={"A": "b"}) == "sent1"
    with pytest.raises(ResendError) as err:
        await api.received_email("bad")
    assert err.value.status == 404 and "re_test" not in str(err.value)
    await api.close()
    assert seen[0][1] == seen[1][1] == "/inboxes/ib/threads"  # retried after 429
    assert ("POST", "/inboxes/ib/threads/t1/emails/e1/reply",
            {"text": "hi", "headers": {"A": "b"}}) in seen


live = pytest.mark.skipif(not os.environ.get("NATS_HUB_TEST_STACK"),
                          reason="needs scripts/dev/with_stack.sh (nats-server + hub-server)")


@live
@pytest.mark.asyncio
async def test_live_email_wakes_echo_worker_and_replies(tmp_path):
    """Fake Resend, real router and worker: the reply contract carries the
    email to echo_worker.py and its result back as the email reply."""
    from nats_connect import connect_nats

    url = os.environ["NATS_URL"]
    agent = f"echo-{uuid.uuid4().hex[:6]}"
    plugin = f"email-bridge-{uuid.uuid4().hex[:6]}"
    worker = subprocess.Popen([sys.executable, "echo_worker.py", "--identity", agent,
                               "--nats-url", url], cwd=REPO_ROOT,
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    nc = await connect_nats(url, name=plugin, identity=plugin)
    try:
        await asyncio.sleep(1.5)  # let the worker subscribe
        mb = Mailbox(address="echo@acs.example.com", agent=agent, inbox_id="ib",
                     allow_from=("ada@example.org",), task_timeout=20)
        cfg = PluginConfig(resend_api_key_file="k", state_path=str(tmp_path / "s.json"),
                           identity=plugin, mailboxes=(mb,))
        api = FakeInboxes()
        bridge = EmailBridge(cfg, api, HubLink(nc, plugin), State(cfg.state_path))
        await poll(bridge)
        api.deliver("ib", "t1", "Ada <ada@example.org>", "ping from email")
        await poll(bridge)
        assert len(api.replies) == 1, api.labels
        assert "ping from email"[::-1] in api.replies[0][3]  # echo reverses the prompt
        assert "acs-replied" in api.labels["t1"]

        # Agent → plugin DM (as the MCP send_direct tool sends it) → email out.
        sender = f"agent-{uuid.uuid4().hex[:6]}"
        cfg2 = PluginConfig(resend_api_key_file="k", state_path=str(tmp_path / "s2.json"),
                            identity=plugin, mailboxes=(Mailbox(
                                address="s@acs.example.com", agent=sender, inbox_id="ib2",
                                may_email=("*@acs.example.com",)),))
        bridge2 = EmailBridge(cfg2, api, HubLink(nc, plugin), State(cfg2.state_path))
        await bridge2.hub.serve_requests(bridge2.send_for_agent)
        snc = await connect_nats(url, name=sender, identity=sender)
        got: asyncio.Future = asyncio.get_running_loop().create_future()

        async def on_reply(msg):
            if not got.done():
                got.set_result(json.loads(msg.data.decode()))

        await snc.subscribe(f"channel.inbox.{sender}", cb=on_reply)
        req = {"email": {"to": ["echo@acs.example.com"], "subject": "hi", "text": "ping"}}
        dm = {"meta": {"id": str(uuid.uuid4()), "from": sender, "channel": f"inbox.{plugin}",
                       "to": plugin, "timestamp": "2026-10-24T00:00:00Z", "kind": "message",
                       "reply_to": None}, "payload": {"message": json.dumps(req)}}
        await snc.publish(f"hub.pub.{sender}.inbox.{plugin}", json.dumps(dm).encode())
        reply = await asyncio.wait_for(got, 10)
        await snc.close()
        assert reply["payload"]["status"] == "done", reply
        assert reply["meta"]["reply_to"] == dm["meta"]["id"]
        assert api.sent[-1][:3] == ("ib2", ["echo@acs.example.com"], "hi")
    finally:
        await nc.close()
        worker.terminate()
        worker.wait(timeout=5)
