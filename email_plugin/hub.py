"""The plugin's only contact with the hub: ordinary DMs.

- ``delegate``: the reply contract (refocus.md §6): subscribe the task channel
  first, DM ``{prompt, task_channel}``, wait for the terminal result; on
  timeout send the §4.2 cancel.
- ``start_session`` / ``send_session`` / ``close_session``: a hub session per
  email thread (docs/EMAIL_PLUGIN.md §4.3), the same wire shapes as
  ``hub-session`` and the MCP session tools. Each turn is a DM to the worker on
  ``session.<id>`` (so a restarted worker gets it from its inbox and can
  resume or report ``unknown session``); its result arrives on that channel.
- ``serve_requests``: agents DM the plugin identity to send email; the plugin
  answers with a §6.6 plain-DM result.
"""
from __future__ import annotations

import asyncio
import json
import logging
import uuid
from dataclasses import dataclass, replace
from datetime import datetime, timezone
from typing import Awaitable, Callable

from hub_delivery import subscribe_inbox, task_channel_for

log = logging.getLogger("email_plugin.hub")
SUB_READY_DELAY = 0.05
API_TIMEOUT = 5.0


@dataclass(frozen=True)
class TaskOutcome:
    task_id: str
    status: str  # done | error | cancelled | timeout
    result: object = None
    error: str | None = None
    session_id: str | None = None

    @property
    def session_gone(self) -> bool:
        """The worker answered that it doesn't hold (and can't resume) the session."""
        return self.status == "error" and "unknown session" in (self.error or "")


def envelope(identity: str, channel: str, payload: dict, *, kind: str = "message",
             to: str | None = None, reply_to: str | None = None) -> dict:
    return {"meta": {"id": str(uuid.uuid4()), "from": identity, "channel": channel,
                     "to": to, "timestamp": datetime.now(timezone.utc).isoformat(),
                     "kind": kind, "reply_to": reply_to},
            "payload": payload}


def is_result(env: dict, task_id: str, worker: str) -> bool:
    meta = env.get("meta", {})
    payload = env.get("payload") or {}
    return (meta.get("kind") == "message" and meta.get("from") == worker
            and (meta.get("reply_to") == task_id or payload.get("task_id") == task_id))


def email_request(payload) -> dict | None:
    """``payload.email``, or the same object as JSON in ``payload.message``
    (what the MCP ``send_direct`` tool sends)."""
    if not isinstance(payload, dict):
        return None
    if isinstance(payload.get("email"), dict):
        return payload["email"]
    msg = payload.get("message")
    if isinstance(msg, str) and msg.lstrip().startswith("{"):
        try:
            inner = json.loads(msg)
        except ValueError:
            return None
        if isinstance(inner, dict) and isinstance(inner.get("email"), dict):
            return inner["email"]
    return None


class HubLink:
    def __init__(self, nc, identity: str):
        self.nc = nc
        self.identity = identity

    async def _publish(self, channel: str, env: dict) -> None:
        await self.nc.publish(f"hub.pub.{self.identity}.{channel}", json.dumps(env).encode())
        await self.nc.flush()

    async def api_request(self, op: str, params: dict) -> dict:
        """Query API (``hub.api.<identity>.<op>``); errors come back as ``ok: false``."""
        req = json.dumps({"op": op, "params": params}).encode()
        try:
            reply = await self.nc.request(f"hub.api.{self.identity}.{op}", req, timeout=API_TIMEOUT)
            return json.loads(reply.data)
        except Exception as e:  # noqa: BLE001 - session records are best effort
            return {"ok": False, "error": str(e)}

    async def _turn(self, worker: str, env: dict, timeout: float,
                    on_start: Callable[[str], None] | None) -> TaskOutcome:
        """Subscribe ``env``'s channel, publish it, wait for the terminal result
        (reply contract); on timeout send the §4.2 cancel."""
        task_id, channel = env["meta"]["id"], env["meta"]["channel"]
        done: asyncio.Future = asyncio.get_running_loop().create_future()

        async def on_msg(msg) -> None:
            try:
                data = json.loads(msg.data.decode())
            except ValueError:
                return
            if not done.done() and is_result(data, task_id, worker):
                done.set_result(data.get("payload") or {})

        sub = await self.nc.subscribe(f"channel.{channel}", cb=on_msg)
        try:
            await self.nc.flush()
            await asyncio.sleep(SUB_READY_DELAY)
            await self._publish(channel, env)
            if on_start:
                on_start(task_id)
            try:
                payload = await asyncio.wait_for(done, timeout)
            except asyncio.TimeoutError:
                cancel = envelope(self.identity, channel, {"action": "cancel", "task_id": task_id},
                                  kind="control", to=worker)
                await self._publish(channel, cancel)
                return TaskOutcome(task_id, "timeout", error=f"no result within {timeout:.0f}s")
        finally:
            await sub.unsubscribe()
        status = payload.get("status") or "done"
        return TaskOutcome(task_id, status, payload.get("result"), payload.get("error"))

    async def delegate(self, worker: str, prompt: str, timeout: float,
                       on_start: Callable[[str], None] | None = None) -> TaskOutcome:
        env = envelope(self.identity, "task", {}, to=worker)
        channel = task_channel_for(env["meta"]["id"])
        env["meta"]["channel"] = channel
        env["payload"] = {"prompt": prompt, "task_channel": channel}
        return await self._turn(worker, env, timeout, on_start)

    async def start_session(self, worker: str, session_id: str, prompt: str, timeout: float,
                            on_start: Callable[[str], None] | None = None,
                            metadata: dict | None = None) -> TaskOutcome:
        """``session_start`` with the first turn's prompt. The session record
        (``session.create``) is written first, so the worker can persist its
        backend context and resume the session after a restart."""
        channel = f"session.{session_id}"
        env = envelope(self.identity, channel, {
            "action": "session_start", "session_id": session_id,
            "session_channel": channel, "prompt": prompt}, to=worker)
        now = env["meta"]["timestamp"]
        resp = await self.api_request("session.create", {
            "session_id": session_id, "orchestrator": self.identity, "worker": worker,
            "status": "active", "created_at": now, "updated_at": now,
            "metadata": {"session_channel": channel, **(metadata or {})}})
        if not resp.get("ok"):
            log.warning("session.create %s failed (worker can't resume it): %s",
                        session_id, resp.get("error"))
        return replace(await self._turn(worker, env, timeout, on_start), session_id=session_id)

    async def send_session(self, worker: str, session_id: str, prompt: str, timeout: float,
                           on_start: Callable[[str], None] | None = None) -> TaskOutcome:
        channel = f"session.{session_id}"
        env = envelope(self.identity, channel, {
            "action": "session_send", "session_id": session_id, "message": prompt}, to=worker)
        return replace(await self._turn(worker, env, timeout, on_start), session_id=session_id)

    async def close_session(self, worker: str, session_id: str) -> None:
        channel = f"session.{session_id}"
        await self._publish(channel, envelope(
            self.identity, channel, {"action": "session_close", "session_id": session_id},
            to=worker))
        resp = await self.api_request("session.update_status",
                                      {"session_id": session_id, "status": "closed"})
        if not resp.get("ok"):
            log.debug("session.update_status %s: %s", session_id, resp.get("error"))

    async def serve_requests(self, handler: Callable[[str, dict], Awaitable[str]]) -> None:
        """DMs to ``inbox.<identity>`` with ``payload.email`` → ``handler(sender,
        email)``; its return value (or exception) goes back as a DM result.
        Reads the durable inbox when the hub offers one (hub_delivery)."""
        async def on_msg(msg) -> None:
            try:
                env = json.loads(msg.data.decode())
            except ValueError:
                return
            meta = env.get("meta", {})
            sender, request = meta.get("from"), email_request(env.get("payload"))
            if meta.get("kind") != "message" or request is None or not sender:
                return
            try:
                result, error, status = await handler(sender, request), None, "done"
            except Exception as e:  # noqa: BLE001 - reported back to the agent
                result, error, status = None, str(e), "error"
            reply = envelope(self.identity, f"inbox.{sender}",
                             {"status": status, "task_id": meta.get("id"),
                              "result": result, "error": error},
                             to=sender, reply_to=meta.get("id"))
            await self._publish(f"inbox.{sender}", reply)

        self._inbox = await subscribe_inbox(self.nc, self.identity, on_msg)
        await self.nc.flush()
