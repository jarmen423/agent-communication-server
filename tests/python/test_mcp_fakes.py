"""In-memory bus + fake workers for MCP server unit tests (no NATS needed).

``FakeBus`` stands in for the nats-py client returned by
``hub_connection.get_nc()``. It routes published envelopes the way the router
does — by ``meta.to`` (→ ``channel.inbox.<to>``) or ``meta.channel`` (→
``channel.<channel>``) — so it works for both the legacy ``hub.send.<ch>`` and
the bound ``hub.pub.<id>.<ch>`` publish subjects (contract §4.1). Query-API
requests are answered from ``bus.api[op]`` using the ``op`` in the request
body, again independent of the subject form.

``FakeWorker`` implements the reply contract (refocus.md §6) and the cancel
contract (refocus-iteration-2.md §4.2).

The module name starts with ``test_`` only to stay inside this task's write
scope; the one test below checks the routing the other tests rely on.
"""
from __future__ import annotations

import asyncio
import json
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "mcp_server"))

import hub_buffers  # noqa: E402
import hub_connection as conn  # noqa: E402


class FakeMsg:
    def __init__(self, subject: str, data: bytes) -> None:
        self.subject = subject
        self.data = data


class FakeSub:
    def __init__(self, bus: "FakeBus", subject: str, cb) -> None:
        self.bus, self.subject, self.cb = bus, subject, cb

    async def unsubscribe(self) -> None:
        if self in self.bus.subs:
            self.bus.subs.remove(self)


def _matches(pattern: str, subject: str) -> bool:
    if pattern.endswith(".>"):
        return subject.startswith(pattern[:-1])
    return pattern == subject


class FakeBus:
    def __init__(self) -> None:
        self.subs: list[FakeSub] = []
        self.is_closed = False
        self.options: dict[str, Any] = {}
        self.published: list[dict] = []  # decoded envelopes, in order
        self.api: dict[str, Callable[[dict], Any]] = {}
        self.requests: dict[str, Callable[[dict], dict]] = {}
        self.api_calls: list[tuple[str, dict]] = []

    async def subscribe(self, subject: str, cb=None) -> FakeSub:
        sub = FakeSub(self, subject, cb)
        self.subs.append(sub)
        return sub

    async def flush(self) -> None:
        pass

    async def deliver(self, subject: str, data: bytes) -> None:
        for sub in list(self.subs):
            if _matches(sub.subject, subject) and sub.cb is not None:
                asyncio.ensure_future(sub.cb(FakeMsg(subject, data)))

    async def publish(self, subject: str, data: bytes) -> None:
        if subject.startswith(("hub.send.", "hub.pub.")):
            env = json.loads(data)
            self.published.append(env)
            meta = env["meta"]
            target = (f"channel.inbox.{meta['to']}" if meta.get("to")
                      else f"channel.{meta['channel']}")
            await self.deliver(target, data)
        else:
            await self.deliver(subject, data)

    async def request(self, subject: str, data: bytes, timeout: float = 1.0):
        body = json.loads(data) if data else {}
        if subject.startswith("hub.api."):
            op = body.get("op")
            self.api_calls.append((op, body.get("params") or {}))
            fn = self.api.get(op)
            if fn is None:
                raise Exception("nats: no responders available for request")
            data_out = fn(body.get("params") or {})
            if isinstance(data_out, dict) and "__error__" in data_out:
                resp = {"ok": False, "error": data_out["__error__"]}
            else:
                resp = {"ok": True, "data": data_out}
        else:
            fn = self.requests.get(subject)
            if fn is None:
                raise Exception("nats: no responders available for request")
            resp = fn(body)
        return FakeMsg(subject, json.dumps(resp).encode())


def install(monkeypatch, identity: str = "t-orch") -> FakeBus:
    """Point hub_connection at a fresh FakeBus and reset the hub state."""
    bus = FakeBus()

    async def get_nc():
        return bus

    monkeypatch.setenv("NATS_HUB_IDENTITY", identity)
    monkeypatch.setattr(conn, "get_nc", get_nc)
    monkeypatch.setattr(hub_buffers, "_state", hub_buffers.HubState())
    monkeypatch.setattr(hub_buffers, "SUB_READY_DELAY", 0)
    return bus


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _env(sender: str, channel: str, payload: dict, *, kind: str,
         to: str | None = None, reply_to: str | None = None) -> dict:
    return {"meta": {"id": str(uuid.uuid4()), "from": sender,
                     "channel": channel, "to": to, "timestamp": now_iso(),
                     "kind": kind, "reply_to": reply_to},
            "payload": payload}


class FakeWorker:
    """Answers task DMs per the reply contract; honors (or ignores) cancel.

    mode: "done" (reply "pong" after ``delay``), "error" (terminal error),
    "silent" (never answers). ``honors_cancel`` controls §4.2 support.
    """

    def __init__(self, bus: FakeBus, identity: str, *, delay: float = 0.0,
                 mode: str = "done", honors_cancel: bool = True) -> None:
        self.bus, self.identity = bus, identity
        self.delay, self.mode, self.honors_cancel = delay, mode, honors_cancel
        self.running: dict[str, tuple[asyncio.Task, dict]] = {}  # id → (task, env)
        self.cancelled: list[str] = []
        self.control_seen: list[dict] = []

    async def start(self) -> "FakeWorker":
        await self.bus.subscribe(f"channel.inbox.{self.identity}", cb=self._on)
        return self

    async def _send(self, env: dict) -> None:
        await self.bus.publish(f"hub.pub.{self.identity}.{env['meta']['channel']}",
                               json.dumps(env).encode())

    async def _result(self, task_env: dict, status: str, result, error) -> None:
        task_id = task_env["meta"]["id"]
        ch = task_env["payload"]["task_channel"]
        await self._send(_env(self.identity, ch, {
            "status": status, "task_id": task_id,
            "result": result, "error": error,
        }, kind="message", reply_to=task_id))

    async def _run(self, task_env: dict) -> None:
        task_id = task_env["meta"]["id"]
        ch = task_env["payload"]["task_channel"]
        await self._send(_env(self.identity, ch, {"status": "working"},
                              kind="status", reply_to=task_id))
        if self.mode == "silent":
            await asyncio.Event().wait()
        await asyncio.sleep(self.delay)
        if self.mode == "error":
            await self._result(task_env, "error", None, "backend: out of credits")
        else:
            await self._result(task_env, "done", "pong", None)
        self.running.pop(task_id, None)

    async def _on(self, msg) -> None:
        env = json.loads(msg.data)
        meta, payload = env["meta"], env.get("payload") or {}
        if meta.get("kind") == "control" and payload.get("action") == "cancel":
            self.control_seen.append(env)
            task_id = payload.get("task_id")
            entry = self.running.pop(task_id, None)
            if entry is None or not self.honors_cancel:
                if entry is not None:
                    self.running[task_id] = entry
                return  # §4.2.3: unknown/finished → ignore, no reply
            task, task_env = entry
            task.cancel()
            self.cancelled.append(task_id)
            await self._result(task_env, "cancelled", None, "cancelled by request")
            return
        if meta.get("kind") == "message" and payload.get("task_channel"):
            task = asyncio.ensure_future(self._run(env))
            self.running[meta["id"]] = (task, env)


def agent_record(identity: str, caps: list[str], metadata: dict | None = None) -> dict:
    return {"identity": identity, "capabilities": caps, "last_seen": now_iso(),
            "registered_at": now_iso(), "metadata": metadata or {}}


def test_fake_bus_routes_like_the_router():
    async def run():
        bus = FakeBus()
        got: dict[str, list] = {"inbox": [], "chan": []}

        async def on_inbox(m):
            got["inbox"].append(json.loads(m.data))

        async def on_chan(m):
            got["chan"].append(json.loads(m.data))

        await bus.subscribe("channel.inbox.w", cb=on_inbox)
        await bus.subscribe("channel.task.x", cb=on_chan)
        dm = _env("o", "task.x", {"p": 1}, kind="message", to="w")
        bc = _env("w", "task.x", {"p": 2}, kind="status")
        await bus.publish("hub.send.task.x", json.dumps(dm).encode())
        await bus.publish("hub.pub.w.task.x", json.dumps(bc).encode())
        await asyncio.sleep(0)
        assert [e["payload"]["p"] for e in got["inbox"]] == [1]
        assert [e["payload"]["p"] for e in got["chan"]] == [2]

    asyncio.run(run())
