"""The plugin's only contact with the hub: ordinary DMs.

- ``delegate``: the reply contract (refocus.md §6): subscribe the task channel
  first, DM ``{prompt, task_channel}``, wait for the terminal result; on
  timeout send the §4.2 cancel.
- ``serve_requests``: agents DM the plugin identity to send email; the plugin
  answers with a §6.6 plain-DM result.
"""
from __future__ import annotations

import asyncio
import json
import logging
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Awaitable, Callable

from hub_delivery import subscribe_inbox, task_channel_for

log = logging.getLogger("email_plugin.hub")
SUB_READY_DELAY = 0.05


@dataclass(frozen=True)
class TaskOutcome:
    task_id: str
    status: str  # done | error | cancelled | timeout
    result: object = None
    error: str | None = None


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

    async def delegate(self, worker: str, prompt: str, timeout: float,
                       on_start: Callable[[str], None] | None = None) -> TaskOutcome:
        env = envelope(self.identity, "task", {}, to=worker)
        task_id = env["meta"]["id"]
        channel = task_channel_for(task_id)
        env["meta"]["channel"] = channel
        env["payload"] = {"prompt": prompt, "task_channel": channel}
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
