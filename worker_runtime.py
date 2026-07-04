"""
Shared nats-hub worker runtime: hub-delegate (one-shot) + hub-session (stateful).

New CLI workers only implement a backend:

    class MyBackend:
        async def run(self, prompt: str, ctx: dict) -> tuple[str, dict]:
            # ctx is per-session state (empty on first turn); return (text, new_ctx)
            ...

    await run_worker(WorkerConfig(identity="...", backend=MyBackend(), ...))
"""

from __future__ import annotations

import asyncio
import json
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Protocol, runtime_checkable

from nats.aio.client import Client as NATSClient
from nats.aio.msg import Msg


def make_envelope(
    from_id: str,
    to_id: str | None,
    channel: str,
    kind: str,
    payload: dict,
    reply_to: str | None = None,
) -> bytes:
    meta = {
        "id": str(uuid.uuid4()),
        "from": from_id,
        "channel": channel,
        "kind": kind,
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }
    if to_id:
        meta["to"] = to_id
    if reply_to:
        meta["reply_to"] = reply_to
    return json.dumps({"meta": meta, "payload": payload}).encode()


@runtime_checkable
class WorkerBackend(Protocol):
    """One method: run a prompt with optional per-session context."""

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        ...


@dataclass
class WorkerConfig:
    identity: str
    backend: WorkerBackend
    nats_url: str = "nats://127.0.0.1:4222"
    log_prefix: str = "worker"
    broadcast_channel: str | None = None
    extra_heartbeat: dict[str, Any] = field(default_factory=dict)


async def run_worker(cfg: WorkerConfig) -> None:
    nc = NATSClient()
    await nc.connect(servers=cfg.nats_url)
    log = cfg.log_prefix
    print(f"[{log}] connected to NATS as {cfg.identity}")

    active_sessions: dict[str, dict[str, Any]] = {}

    async def publish(channel: str, kind: str, payload: dict, reply_to: str | None = None) -> None:
        await nc.publish(
            f"hub.send.{channel}",
            make_envelope(cfg.identity, None, channel, kind, payload, reply_to=reply_to),
        )

    # ── One-shot (hub-delegate) ─────────────────────────────────

    async def process_oneshot(msg: Msg) -> None:
        try:
            envelope = json.loads(msg.data.decode())
        except Exception as e:
            print(f"[{log}] decode error: {e}")
            return

        meta = envelope.get("meta", {})
        payload = envelope.get("payload", {})

        prompt = payload.get("prompt") or payload.get("text") or payload.get("command")
        if not prompt:
            return

        task_channel = payload.get("task_channel") or meta.get("reply_to")
        if not task_channel:
            print(f"[{log}] oneshot: no task_channel, skipping")
            return

        task_id = meta.get("id")
        print(f"[{log}] oneshot on {task_channel}: {prompt[:80]}...")

        await publish(task_channel, "status", {"status": "working"})
        try:
            result_text, _ = await cfg.backend.run(prompt, {})
            await publish(
                task_channel,
                "message",
                {"result": result_text, "task_id": task_id, "status": "done"},
                reply_to=task_id,
            )
            await publish(task_channel, "status", {"status": "done"})
        except Exception as err:
            print(f"[{log}] oneshot failed: {err}")
            await publish(
                task_channel,
                "message",
                {"error": str(err), "task_id": task_id, "status": "error"},
                reply_to=task_id,
            )
            await publish(task_channel, "status", {"status": "error"})

    # ── Stateful sessions (hub-session) ─────────────────────────

    async def run_session_turn(
        session_id: str,
        session_channel: str,
        prompt: str,
        task_id: str | None,
    ) -> None:
        session = active_sessions.get(session_id)
        if not session:
            return

        await publish(session_channel, "status", {"status": "working"})
        try:
            backend_ctx = session.get("backend_ctx") or {}
            result_text, new_ctx = await cfg.backend.run(prompt, backend_ctx)
            session["backend_ctx"] = new_ctx
            await publish(
                session_channel,
                "message",
                {"result": result_text, "task_id": task_id, "status": "done"},
                reply_to=task_id,
            )
            await publish(session_channel, "status", {"status": "idle"})
        except Exception as err:
            print(f"[{log}] session {session_id} failed: {err}")
            await publish(
                session_channel,
                "message",
                {"error": str(err), "task_id": task_id, "status": "error"},
                reply_to=task_id,
            )
            await publish(session_channel, "status", {"status": "error"})

    async def process_session_msg(msg: Msg) -> None:
        try:
            envelope = json.loads(msg.data.decode())
        except Exception as e:
            print(f"[{log}] decode error: {e}")
            return

        meta = envelope.get("meta", {})
        payload = envelope.get("payload", {})
        action = payload.get("action")
        session_id = payload.get("session_id") or meta.get("channel", "").replace("session.", "")

        if action == "session_start":
            session_channel = payload.get("session_channel", f"session.{session_id}")
            sender = meta.get("from", "unknown")
            prompt = payload.get("prompt")
            print(f"[{log}] session_start: {session_id} from {sender}")

            async def session_callback(smsg: Msg) -> None:
                await process_session_msg(smsg)

            sub = await nc.subscribe(f"channel.{session_channel}", cb=session_callback)
            active_sessions[session_id] = {
                "channel": session_channel,
                "subscription": sub,
                "backend_ctx": {"_session_id": session_id},
                "sender": sender,
            }
            await publish(session_channel, "status", {"status": "ready"})
            if prompt:
                await run_session_turn(session_id, session_channel, prompt, meta.get("id"))

        elif action == "session_send":
            if session_id not in active_sessions:
                print(f"[{log}] session_send unknown session {session_id}")
                return
            session = active_sessions[session_id]
            message = payload.get("message") or payload.get("prompt") or payload.get("text")
            if not message:
                return
            print(f"[{log}] session_send: {session_id}: {message[:80]}...")
            await run_session_turn(session_id, session["channel"], message, meta.get("id"))

        elif action == "session_close":
            if session_id not in active_sessions:
                return
            session = active_sessions.pop(session_id)
            print(f"[{log}] session_close: {session_id}")
            await session["subscription"].unsubscribe()
            await publish(session["channel"], "status", {"status": "closed"})

    # ── Inbox router ────────────────────────────────────────────

    async def inbox_callback(msg: Msg) -> None:
        try:
            envelope = json.loads(msg.data.decode())
            action = envelope.get("payload", {}).get("action")
            if action in ("session_start", "session_send", "session_close"):
                await process_session_msg(msg)
            else:
                await process_oneshot(msg)
        except Exception as e:
            print(f"[{log}] handler error: {e}")

    inbox_subject = f"channel.inbox.{cfg.identity}"
    await nc.subscribe(inbox_subject, cb=inbox_callback)
    print(f"[{log}] subscribed to {inbox_subject} (oneshot + sessions)")

    if cfg.broadcast_channel:
        async def broadcast_callback(msg: Msg) -> None:
            await inbox_callback(msg)

        await nc.subscribe(f"channel.{cfg.broadcast_channel}", cb=broadcast_callback)

    async def heartbeat() -> None:
        while True:
            await asyncio.sleep(30)
            payload = {"identity": cfg.identity, **cfg.extra_heartbeat}
            await nc.publish(
                "hub.presence",
                make_envelope(cfg.identity, None, "hub.presence", "status", payload),
            )

    asyncio.create_task(heartbeat())
    print(f"[{log}] ready")

    try:
        while True:
            await asyncio.sleep(1)
    except (KeyboardInterrupt, asyncio.CancelledError):
        pass
    finally:
        await nc.close()