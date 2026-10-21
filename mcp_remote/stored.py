"""Stateless reads from JetStream for the remote MCP endpoint.

Nothing is buffered in process memory between tool calls, so any replica
answers any call the same way:

- task results and session replies are read back from the ``HUB_TASKS``
  stream with ``DIRECT.GET`` (next message on a subject after a sequence);
  the stream sequence is the paging cursor;
- the inbox is the identity's durable ``inbox-<id>`` pull consumer, which
  hub-server creates on ``hub.delivery.<id>.inbox``; its ack floor is the
  read position, shared by every replica.

Both need ``hub-server --durable``; on a core hub they raise ``NotDurable``.
"""

from __future__ import annotations

import asyncio
import json
from typing import Any

import nats.errors

import hub_delivery
from hub_primitives import is_task_result, summarize_env

REQUEST_TIMEOUT = 3.0
MAX_SCAN = 20_000  # messages read back for one task channel (stream keeps 10k per subject)


class NotDurable(RuntimeError):
    def __init__(self, what: str) -> None:
        super().__init__(f"{what} needs a durable hub (hub-server --durable): "
                         "this endpoint reads results and inboxes from JetStream")


async def read_stored(nc: Any, subject: str, *, after_seq: int = 0,
                      limit: int = 500) -> list[tuple[int, dict]]:
    """``(stream_seq, envelope)`` stored on ``subject`` after ``after_seq``,
    oldest first, at most ``limit``. Undecodable messages are skipped."""
    stream = hub_delivery.tasks_stream()
    out: list[tuple[int, dict]] = []
    seq = after_seq
    while len(out) < limit:
        req = json.dumps({"seq": seq + 1, "next_by_subj": subject}).encode()
        try:
            msg = await nc.request(f"$JS.API.DIRECT.GET.{stream}", req, timeout=REQUEST_TIMEOUT)
        except nats.errors.NoRespondersError as e:
            raise NotDurable("reading stored task/session channels") from e
        headers = msg.headers or {}
        status = headers.get("Status")
        if status == "404":
            break
        if status:
            raise RuntimeError(f"JetStream direct get on {stream} failed: "
                               f"{status} {headers.get('Description', '')}".strip())
        try:
            seq = int(headers.get("Nats-Sequence", "0"))
        except ValueError:
            break
        try:
            env = json.loads(msg.data.decode())
        except (UnicodeDecodeError, ValueError):
            continue
        if isinstance(env, dict):
            out.append((seq, env))
    return out


class TaskView:
    """What the stored task channel says about one task (C5: only
    ``worker``'s correlated message counts as the result)."""

    def __init__(self, task_id: str, worker: str) -> None:
        self.task_id, self.worker = task_id, worker
        self.channel = hub_delivery.task_channel_for(task_id)
        self.last_seq = 0
        self.scanned = 0
        self.last_status: str | None = None
        self.events: list[dict] = []
        self.result: dict | None = None

    async def update(self, nc: Any) -> None:
        """Read whatever was stored since the last update."""
        while self.result is None and self.scanned < MAX_SCAN:
            batch = await read_stored(nc, f"channel.{self.channel}",
                                      after_seq=self.last_seq, limit=500)
            for seq, env in batch:
                self.last_seq = seq
                self.scanned += 1
                self._feed(env)
                if self.result is not None:
                    break
            if len(batch) < 500:
                break

    def _feed(self, env: dict) -> None:
        meta = env.get("meta") or {}
        kind = meta.get("kind")
        if kind in ("status", "event"):
            payload = env.get("payload") or {}
            if kind == "status" and isinstance(payload.get("status"), str):
                self.last_status = payload["status"]
            self.events = (self.events + [{"from": meta.get("from"), "kind": kind,
                                           "payload": payload}])[-20:]
        elif is_task_result(env, self.task_id, self.worker):
            self.result = env

    @property
    def state(self) -> str:
        if self.result is None:
            return "running"
        status = (self.result.get("payload") or {}).get("status")
        return status if status in ("error", "cancelled") else "done"

    async def wait(self, nc: Any, wait_secs: float, poll: float = 0.5) -> None:
        loop = asyncio.get_running_loop()
        deadline = loop.time() + wait_secs
        await self.update(nc)
        while self.result is None and loop.time() < deadline:
            await asyncio.sleep(min(poll, max(0.0, deadline - loop.time())))
            await self.update(nc)


async def read_inbox(nc: Any, identity: str, *, limit: int, wait_secs: float,
                     max_chars: int) -> dict:
    """Pull up to ``limit`` DMs from the durable inbox and ack them. A DM
    that doesn't fit in ``max_chars`` is naked, so the next read gets it."""
    target = await hub_delivery._find_consumer(nc, identity)
    if target is None:
        raise NotDurable("read_inbox")
    stream, consumer = target
    psub = await nc.jetstream().pull_subscribe_bind(consumer, stream=stream)
    messages: list[dict] = []
    naked = 0
    try:
        try:
            msgs = await psub.fetch(limit, timeout=max(wait_secs, 1.0))
        except (nats.errors.TimeoutError, asyncio.TimeoutError):
            msgs = []
        used = 0
        for msg in msgs:
            meta = msg.metadata
            try:
                env = json.loads(msg.data.decode())
            except (UnicodeDecodeError, ValueError):
                await msg.ack()  # never deliverable; don't loop on it
                continue
            item = summarize_env({"seq": meta.sequence.stream, "env": env})
            size = len(json.dumps(item, ensure_ascii=False, default=str))
            if size > max_chars:
                item["payload"] = {"truncated": True, "size_chars": size,
                                   "hint": "too large for one tool result; "
                                           f"see get_history(channel='{item.get('channel')}')"}
                size = len(json.dumps(item, ensure_ascii=False, default=str))
            if messages and used + size > max_chars:
                await msg.nak()  # redelivered at once to the next read
                naked += 1
                continue
            used += size
            messages.append(item)
            await msg.ack()
        # num_pending: stored after the last fetched message.
        pending = (msgs[-1].metadata.num_pending if msgs else 0) + naked
    finally:
        try:
            await psub.unsubscribe()
        except Exception:  # noqa: BLE001 - connection gone; the consumer persists
            pass
    return {"messages": messages, "more_pending": pending}


async def read_session(nc: Any, session_id: str, *, after_seq: int, limit: int,
                       wait_secs: float, poll: float = 0.5) -> dict:
    subject = f"channel.session.{session_id}"
    loop = asyncio.get_running_loop()
    deadline = loop.time() + wait_secs
    items = await read_stored(nc, subject, after_seq=after_seq, limit=limit)
    while not items and loop.time() < deadline:
        await asyncio.sleep(min(poll, max(0.0, deadline - loop.time())))
        items = await read_stored(nc, subject, after_seq=after_seq, limit=limit)
    return {
        "messages": [summarize_env({"seq": seq, "env": env}) for seq, env in items],
        "next_seq": items[-1][0] if items else after_seq,
    }
