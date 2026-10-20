"""Durable delivery for Python clients (``hub-server --durable``; docs/DELIVERY.md).

Shared by the worker runtime and the MCP server (vendored into ``mcp_server/``
and the plugins by ``scripts/dev/sync_plugins.sh``, like ``nats_connect.py``).

- :func:`subscribe_inbox` reads ``channel.inbox.<identity>`` from the identity's
  JetStream inbox consumer when the hub is durable (DMs sent while offline
  arrive first, in order; each is acked after the callback ran), else it is
  a plain core subscription. Mode: ``NATS_HUB_DURABLE_INBOX`` = ``auto``
  (default) | ``off`` | ``require``.
- :class:`SeenIds` drops redeliveries by ``meta.id`` (at-least-once → once).
- :class:`ChannelCatchUp` reads a stored task/session/wave channel back by
  sequence (``DIRECT.GET``) so a reconnecting delegator still gets the result.

Agents never create JetStream objects: hub-server creates the inbox consumer
when asked on ``hub.delivery.<identity>.inbox``. On a hub without
``--durable`` nobody answers ("no responders"), so ``auto`` falls back at once.
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
from collections import OrderedDict
from typing import Any, Awaitable, Callable

ATTACH_TIMEOUT = 2.0
FETCH_BATCH = 32
FETCH_WAIT = 5.0
CATCH_UP_BATCH = 500
DEFAULT_SEEN = 4096

Callback = Callable[[Any], Awaitable[None]]


def _log(msg: str) -> None:
    print(f"[hub-delivery] {msg}", file=sys.stderr)


def _name(env: str, default: str) -> str:
    v = os.environ.get(env, "")
    ok = v and all(c.isalnum() or c in "_-" for c in v)
    return v if ok else default


def inbox_stream() -> str:
    return _name("NATS_HUB_INBOX_STREAM", "HUB_INBOX")


def tasks_stream() -> str:
    return _name("NATS_HUB_TASKS_STREAM", "HUB_TASKS")


def inbox_mode() -> str:
    v = os.environ.get("NATS_HUB_DURABLE_INBOX", "").strip().lower()
    if v in ("0", "off", "false", "no"):
        return "off"
    if v in ("1", "on", "true", "yes", "require"):
        return "require"
    return "auto"


def task_channel_for(task_id: str) -> str:
    """``task.<first 8 id chars>`` — the channel a delegator uses for a task
    envelope id (same rule as Rust ``task_channel_for``), so a reconnecting
    delegator can find it again from the id alone."""
    short = "".join(c for c in task_id if c.isascii() and (c.isalnum() or c == "_"))[:8]
    return f"task.{short}"


class SeenIds:
    """Bounded FIFO set of envelope ids."""

    def __init__(self, cap: int = DEFAULT_SEEN) -> None:
        self.cap = max(1, cap)
        self._ids: OrderedDict[str, None] = OrderedDict()

    def first_sight(self, env_id: Any) -> bool:
        """True the first time ``env_id`` is seen (non-str ids always pass)."""
        if not isinstance(env_id, str) or not env_id:
            return True
        if env_id in self._ids:
            return False
        self._ids[env_id] = None
        if len(self._ids) > self.cap:
            self._ids.popitem(last=False)
        return True


def envelope_id(data: bytes) -> str | None:
    try:
        env = json.loads(data.decode())
    except Exception:  # noqa: BLE001 - the callback reports bad envelopes
        return None
    meta = env.get("meta") if isinstance(env, dict) else None
    return meta.get("id") if isinstance(meta, dict) else None


class InboxSubscription:
    """Handle returned by :func:`subscribe_inbox`."""

    def __init__(self, durable: bool, sub: Any = None) -> None:
        self.durable = durable
        self.stopped = False
        self._sub = sub
        self._task: asyncio.Task | None = None

    async def unsubscribe(self) -> None:
        self.stopped = True
        if self._task is not None:
            # nats-py's fetch can swallow a cancel; the flag stops the loop.
            self._task.cancel()
            await asyncio.wait([self._task], timeout=FETCH_WAIT + 1)
        if self._sub is not None:
            try:
                await self._sub.unsubscribe()
            except Exception:  # noqa: BLE001 - connection already closed
                pass


async def _find_consumer(nc: Any, identity: str) -> tuple[str, str] | None:
    """(stream, consumer) of our durable inbox, or None (core hub)."""
    stream, consumer = inbox_stream(), f"inbox-{identity}"
    try:
        reply = await nc.request(f"hub.delivery.{identity}.inbox", b"{}", timeout=ATTACH_TIMEOUT)
        body = json.loads(reply.data)
        if body.get("ok"):
            stream = body.get("stream") or stream
            consumer = body.get("consumer") or consumer
        else:
            _log(f"inbox attach refused: {body.get('error')}")
    except Exception:  # noqa: BLE001 - no responders / timeout: try an existing consumer
        pass
    try:
        js = nc.jetstream(timeout=ATTACH_TIMEOUT)
        await js.consumer_info(stream, consumer, timeout=ATTACH_TIMEOUT)
    except Exception:  # noqa: BLE001 - no JetStream / no stream / no consumer
        return None
    return stream, consumer


async def subscribe_inbox(nc: Any, identity: str, cb: Callback,
                          *, mode: str | None = None) -> InboxSubscription:
    """Subscribe ``cb`` to ``channel.inbox.<identity>`` (durable when offered).

    ``cb`` gets nats-py ``Msg`` objects either way (``msg.subject`` is
    ``channel.inbox.<identity>``); redeliveries are dropped by ``meta.id``."""
    subject = f"channel.inbox.{identity}"
    seen = SeenIds()
    mode = mode or inbox_mode()
    target = None if mode == "off" else await _find_consumer(nc, identity)
    if target is None and mode == "require":
        raise RuntimeError(f"NATS_HUB_DURABLE_INBOX=require but the hub offers no durable "
                           f"inbox for {identity} (is hub-server running with --durable?)")
    if target is None:
        async def live(msg: Any) -> None:
            if seen.first_sight(envelope_id(msg.data)):
                await cb(msg)

        return InboxSubscription(False, sub=await nc.subscribe(subject, cb=live))

    stream, consumer = target
    js = nc.jetstream()
    psub = await js.pull_subscribe_bind(consumer, stream=stream)
    handle = InboxSubscription(True, sub=psub)
    handle._task = asyncio.ensure_future(_pump(nc, psub, cb, seen, identity, handle))
    _log(f"{identity}: reading durable inbox {stream}/{consumer}")
    return handle


async def _pump(nc: Any, psub: Any, cb: Callback, seen: SeenIds, identity: str,
                handle: InboxSubscription) -> None:
    while not handle.stopped and not nc.is_closed:
        try:
            msgs = await psub.fetch(FETCH_BATCH, timeout=FETCH_WAIT)
        except asyncio.CancelledError:
            raise
        except asyncio.TimeoutError:
            continue
        except Exception as e:  # noqa: BLE001 - disconnected / consumer gone: retry
            if nc.is_closed or handle.stopped:
                return
            if type(e).__name__ != "TimeoutError":
                _log(f"{identity}: inbox fetch failed ({e!r}); retrying")
            await asyncio.sleep(1.0)
            continue
        for msg in msgs:
            if handle.stopped:
                return  # unacked: redelivered to the next reader
            if seen.first_sight(envelope_id(msg.data)):
                try:
                    await cb(msg)
                except Exception as e:  # noqa: BLE001 - one bad DM must not stop the inbox
                    _log(f"{identity}: inbox callback failed: {e!r}")
            try:
                await msg.ack()
            except Exception:  # noqa: BLE001 - redelivered later, deduped then
                pass


class ChannelCatchUp:
    """Incremental read of one stored channel (``channel.<name>``) from the
    tasks stream, deduped against a live subscription via :meth:`first_sight`."""

    def __init__(self, channel: str) -> None:
        self.subject = f"channel.{channel}"
        self.stream = tasks_stream()
        self.last_seq = 0
        self.seen = SeenIds()
        self.available: bool | None = None

    def first_sight(self, env: dict) -> bool:
        return self.seen.first_sight((env.get("meta") or {}).get("id"))

    async def poll(self, nc: Any) -> list[dict]:
        """Stored, not yet seen envelopes after the last poll, oldest first.
        ``[]`` (and ``available = False``) on a hub without the stream."""
        if self.available is False:
            return []
        out: list[dict] = []
        while len(out) < CATCH_UP_BATCH:
            req = json.dumps({"seq": self.last_seq + 1, "next_by_subj": self.subject}).encode()
            try:
                msg = await nc.request(f"$JS.API.DIRECT.GET.{self.stream}", req, timeout=ATTACH_TIMEOUT)
            except Exception:  # noqa: BLE001 - no responders / timeout: core hub
                if self.available is None:
                    self.available = False
                return out
            headers = msg.headers or {}
            status = headers.get("Status")
            if status == "404":
                break
            if status:
                return out
            self.available = True
            try:
                self.last_seq = int(headers.get("Nats-Sequence", "0"))
            except ValueError:
                return out
            try:
                env = json.loads(msg.data.decode())
            except Exception:  # noqa: BLE001
                continue
            if isinstance(env, dict) and self.first_sight(env):
                out.append(env)
        if self.available is None:
            self.available = True
        return out
