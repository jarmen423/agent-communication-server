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
import signal
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any, Protocol, runtime_checkable

from nats.aio.client import Client as NATSClient  # noqa: F401  (re-exported)
from nats.aio.msg import Msg

from nats_connect import connect_nats
from worker_events import execute_with_events, publish_event as emit_event, run_to_result

# Payload keys a one-shot prompt may arrive under (bridges send "message").
PROMPT_KEYS = ("prompt", "text", "command", "message")
# Kinds that are progress/bookkeeping, never tasks.
NON_TASK_KINDS = ("status", "event", "control")


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
    # Auth/TLS kwargs forwarded to nats_connect.connect_nats(). When None,
    # connect_nats resolves from NATS_* env vars. Built by callers from CLI.
    nats_auth: dict[str, Any] | None = None
    capabilities: list[str] = field(default_factory=lambda: ["worker"])
    heartbeat_secs: float = 30.0


def extract_prompt(payload: dict) -> str | None:
    for key in PROMPT_KEYS:
        value = payload.get(key)
        if isinstance(value, str) and value:
            return value
    return None


def install_stop_signals(task: asyncio.Task) -> None:
    """SIGTERM/SIGHUP → cancel ``task`` so run_worker closes NATS cleanly.

    Skipped for any signal that already has a handler, so an entrypoint that
    installs its own (e.g. forwarding to CLI process groups) keeps it.
    """
    loop = asyncio.get_running_loop()
    for name in ("SIGTERM", "SIGHUP"):
        sig = getattr(signal, name, None)
        if sig is None or signal.getsignal(sig) is not signal.SIG_DFL:
            continue
        try:
            loop.add_signal_handler(sig, task.cancel)
        except (NotImplementedError, RuntimeError, ValueError):
            pass  # non-main thread or unsupported platform


def is_task_result(payload: dict) -> bool:
    """Results carry task_id + status; never answer one (avoids DM ping-pong)."""
    return "task_id" in payload and "status" in payload


async def run_worker(cfg: WorkerConfig) -> None:
    connect_kwargs: dict[str, Any] = {"name": cfg.identity}
    if cfg.nats_auth:
        connect_kwargs.update(cfg.nats_auth)
    nc = await connect_nats(cfg.nats_url, **connect_kwargs)
    log = cfg.log_prefix
    print(f"[{log}] connected to NATS as {cfg.identity}")

    active_sessions: dict[str, dict[str, Any]] = {}
    inbox_subject = f"channel.inbox.{cfg.identity}"

    async def publish(channel: str, kind: str, payload: dict, reply_to: str | None = None) -> None:
        await nc.publish(
            f"hub.send.{channel}",
            make_envelope(cfg.identity, None, channel, kind, payload, reply_to=reply_to),
        )

    async def publish_event(
        channel: str, event_type: str, data: dict, reply_to: str | None = None
    ) -> None:
        await emit_event(publish, channel, event_type, data, reply_to=reply_to)

    # ── One-shot (hub-delegate) ─────────────────────────────────

    async def process_oneshot(msg: Msg) -> None:
        try:
            envelope = json.loads(msg.data.decode())
        except Exception as e:
            print(f"[{log}] decode error: {e}")
            return

        meta = envelope.get("meta") or {}
        payload = envelope.get("payload")
        if not isinstance(payload, dict) or meta.get("kind") in NON_TASK_KINDS:
            return
        if is_task_result(payload):
            return

        prompt = extract_prompt(payload)
        if not prompt:
            return

        task_id = meta.get("id")
        task_channel = payload.get("task_channel")
        if not task_channel:
            # Rule 6: plain DM (e.g. from a human bridge) → reply by DM to the
            # sender with the same result shape. Only for our own inbox, so
            # chatter on a --broadcast channel doesn't trigger replies.
            sender = meta.get("from")
            if not sender or msg.subject != inbox_subject:
                return
            print(f"[{log}] oneshot DM from {sender}: {prompt[:80]}...")
            result = await run_to_result(cfg.backend, prompt, {}, task_id)
            channel = meta.get("channel") or f"inbox.{sender}"
            await nc.publish(
                f"hub.send.{channel}",
                make_envelope(cfg.identity, sender, channel, "message", result, reply_to=task_id),
            )
            return

        print(f"[{log}] oneshot on {task_channel}: {prompt[:80]}...")

        await execute_with_events(
            publish=publish,
            publish_event_fn=publish_event,
            backend=cfg.backend,
            channel=task_channel,
            prompt=prompt,
            ctx={},
            task_id=task_id,
            working_status="working",
            done_status="done",
        )

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

        backend_ctx = session.get("backend_ctx") or {}
        outcome = await execute_with_events(
            publish=publish,
            publish_event_fn=publish_event,
            backend=cfg.backend,
            channel=session_channel,
            prompt=prompt,
            ctx=backend_ctx,
            task_id=task_id,
            working_status="working",
            done_status="idle",
            wave_channel=session.get("wave_channel"),
            verify_cmd=session.get("verify_cmd"),
        )
        if outcome is not None:
            _, new_ctx = outcome
            session["backend_ctx"] = new_ctx
        else:
            print(f"[{log}] session {session_id} failed")

    async def process_session_msg(msg: Msg) -> None:
        try:
            envelope = json.loads(msg.data.decode())
        except Exception as e:
            print(f"[{log}] decode error: {e}")
            return

        meta = envelope.get("meta", {})
        payload = envelope.get("payload", {})
        action = payload.get("action")
        session_id = payload.get("session_id")
        if not session_id:
            ch = meta.get("channel", "")
            if ".task." in ch:
                session_id = ch.rsplit(".task.", 1)[-1]
            elif ch.startswith("session."):
                session_id = ch[len("session.") :]

        if action == "session_start":
            session_channel = (
                payload.get("session_channel")
                or payload.get("channel")
                or f"session.{session_id}"
            )
            wave_id = payload.get("wave_id")
            wave_channel = f"wave.{wave_id}" if wave_id else None
            sender = meta.get("from", "unknown")
            prompt = payload.get("prompt")
            print(f"[{log}] session_start: {session_id} from {sender} on {session_channel}")

            async def session_callback(smsg: Msg) -> None:
                await process_session_msg(smsg)

            sub = await nc.subscribe(f"channel.{session_channel}", cb=session_callback)
            wave_sub = None
            if wave_channel:
                async def wave_callback(_msg: Msg) -> None:
                    pass

                wave_sub = await nc.subscribe(f"channel.{wave_channel}", cb=wave_callback)
                print(f"[{log}] subscribed to wave channel {wave_channel}")

            active_sessions[session_id] = {
                "channel": session_channel,
                "subscription": sub,
                "wave_subscription": wave_sub,
                "wave_channel": wave_channel,
                "verify_cmd": payload.get("verify_cmd"),
                "write_scope": payload.get("write_scope"),
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
            if session.get("wave_subscription"):
                await session["wave_subscription"].unsubscribe()
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

    await nc.subscribe(inbox_subject, cb=inbox_callback)
    print(f"[{log}] subscribed to {inbox_subject} (oneshot + sessions)")

    if cfg.broadcast_channel:
        async def broadcast_callback(msg: Msg) -> None:
            await inbox_callback(msg)

        await nc.subscribe(f"channel.{cfg.broadcast_channel}", cb=broadcast_callback)

    async def send_heartbeat() -> None:
        payload = {"identity": cfg.identity, **cfg.extra_heartbeat}
        await nc.publish(
            "hub.presence",
            make_envelope(cfg.identity, None, "hub.presence", "status", payload),
        )

    async def heartbeat_loop() -> None:
        while True:
            await asyncio.sleep(cfg.heartbeat_secs)
            try:
                await send_heartbeat()
            except Exception as e:  # noqa: BLE001 - keep beating
                print(f"[{log}] heartbeat failed: {e}")

    # Graceful stop must be in place before anyone can see us (and signal us).
    current = asyncio.current_task()
    if current is not None:
        install_stop_signals(current)

    heartbeat_task: asyncio.Task | None = None
    try:
        # Announce ourselves right after subscribing, so `hub-agents` shows
        # the worker immediately instead of after the first heartbeat interval.
        await nc.publish(
            "hub.register",
            make_envelope(
                cfg.identity,
                None,
                "system",
                "control",
                {"identity": cfg.identity, "capabilities": list(cfg.capabilities)},
            ),
        )
        await send_heartbeat()
        await nc.flush()
        heartbeat_task = asyncio.create_task(heartbeat_loop())
        print(f"[{log}] ready")

        while True:
            await asyncio.sleep(1)
    except (KeyboardInterrupt, asyncio.CancelledError):
        pass
    finally:
        if heartbeat_task is not None:
            heartbeat_task.cancel()
        await nc.close()