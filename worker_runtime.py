"""
Shared nats-hub worker runtime: hub-delegate (one-shot) + hub-session (stateful).

New CLI workers only implement a backend:

    class MyBackend:
        async def run(self, prompt: str, ctx: dict) -> tuple[str, dict]:
            # ctx is per-session state (empty on first turn); return (text, new_ctx)
            ...

    await run_worker(WorkerConfig(identity="...", backend=MyBackend(), ...))

Cancel (refocus-iteration-2.md §4.2): a DM ``kind=control`` with payload
``{"action": "cancel", "task_id": <task envelope id>}`` stops that turn
(queued or running) and yields one ``status: "cancelled"`` result. Inbox
envelopes are queued and run one at a time by a dispatcher, so the inbox
subscription itself never blocks and a cancel is handled while a task runs.
"""

from __future__ import annotations

import asyncio
import json
from dataclasses import dataclass, field
from typing import Any, Protocol, runtime_checkable

from nats.aio.client import Client as NATSClient  # noqa: F401  (re-exported)
from nats.aio.msg import Msg

from nats_connect import connect_nats
from worker_backends import session_resume
from worker_backends.envelope import (  # noqa: F401  (re-exported: tests, bridges)
    PROMPT_KEYS,
    extract_prompt,
    install_stop_signals,
    is_task_result,
    make_envelope,
)
from worker_backends.inbox import NON_TASK_KINDS, InboxDispatcher
from worker_backends.task_registry import TaskRegistry
from worker_events import (
    execute_with_events,
    publish_cancelled,
    publish_event as emit_event,
    run_to_result,
)

# Seconds between the first re-announcements (then every heartbeat_secs).
ANNOUNCE_BACKOFF = (1.0, 2.0, 4.0, 8.0)


async def _ignore(_msg: Msg) -> None:
    return None


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
    # Resume sessions from backend_ctx persisted through the hub API (see
    # worker_backends/session_resume.py). Off unless NATS_HUB_SESSION_RESUME=1.
    session_resume: bool = field(default_factory=session_resume.resume_enabled)


async def run_worker(cfg: WorkerConfig) -> None:
    connect_kwargs: dict[str, Any] = {"name": cfg.identity}
    if cfg.nats_auth:
        connect_kwargs.update(cfg.nats_auth)
    nc = await connect_nats(cfg.nats_url, **connect_kwargs)
    log = cfg.log_prefix
    print(f"[{log}] connected to NATS as {cfg.identity}")

    active_sessions: dict[str, dict[str, Any]] = {}
    inbox_subject = f"channel.inbox.{cfg.identity}"
    registry = TaskRegistry()  # queued + running turns, by task envelope id

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
            result = await run_to_result(cfg.backend, prompt, {}, task_id, registry.get(task_id))
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
            cancel=registry.get(task_id),
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
        cancel = registry.get(task_id) or registry.register(task_id)
        try:
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
                cancel=cancel,
            )
        finally:
            registry.finish(task_id)
        if outcome is not None:
            _, new_ctx = outcome
            session["backend_ctx"] = new_ctx
            if cfg.session_resume:
                await session_resume.save_backend_ctx(api_request, session_id, new_ctx)
        else:
            print(f"[{log}] session {session_id} failed or was cancelled")

    async def open_session(
        session_id: str,
        session_channel: str,
        sender: str,
        payload: dict,
        backend_ctx: dict[str, Any] | None = None,
    ) -> None:
        sub = await nc.subscribe(f"channel.{session_channel}", cb=process_session_msg)
        wave_id = payload.get("wave_id")
        wave_channel = f"wave.{wave_id}" if wave_id else None
        wave_sub = None
        if wave_channel:
            wave_sub = await nc.subscribe(f"channel.{wave_channel}", cb=_ignore)
            print(f"[{log}] subscribed to wave channel {wave_channel}")

        active_sessions[session_id] = {
            "channel": session_channel,
            "subscription": sub,
            "wave_subscription": wave_sub,
            "wave_channel": wave_channel,
            "verify_cmd": payload.get("verify_cmd"),
            "write_scope": payload.get("write_scope"),
            "backend_ctx": {**(backend_ctx or {}), "_session_id": session_id},
            "sender": sender,
        }

    async def resume_session(session_id: str, session_channel: str, sender: str) -> bool:
        """Session resume (stub, behind cfg.session_resume): rebuild a session
        this process doesn't hold from the backend_ctx persisted via the API."""
        if not cfg.session_resume or session_id in active_sessions:
            return session_id in active_sessions
        ctx = await session_resume.fetch_backend_ctx(api_request, session_id)
        if ctx is None:
            return False
        await open_session(session_id, session_channel, sender, {}, ctx)
        print(f"[{log}] resumed session {session_id} from persisted backend_ctx")
        return True

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
            sender = meta.get("from", "unknown")
            prompt = payload.get("prompt")
            print(f"[{log}] session_start: {session_id} from {sender} on {session_channel}")
            await open_session(session_id, session_channel, sender, payload)
            await publish(session_channel, "status", {"status": "ready"})
            if prompt:
                await run_session_turn(session_id, session_channel, prompt, meta.get("id"))

        elif action == "session_send":
            channel = meta.get("channel") or f"session.{session_id}"
            if not await resume_session(session_id, channel, meta.get("from", "unknown")):
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

    async def route(msg: Msg, envelope: dict) -> None:
        action = (envelope.get("payload") or {}).get("action")
        if action in ("session_start", "session_send", "session_close"):
            await process_session_msg(msg)
        else:
            await process_oneshot(msg)

    async def report_cancelled(envelope: dict) -> None:
        """A queued delegated task was cancelled before it started."""
        task_id = (envelope.get("meta") or {}).get("id")
        await publish_cancelled(publish, publish_event, envelope["payload"]["task_channel"],
                                task_id, by="cancel")

    inbox = InboxDispatcher(inbox_subject, registry, route, report_cancelled, log)

    async def api_request(op: str, params: dict) -> dict:
        req = json.dumps({"op": op, "params": params}).encode()
        reply = await nc.request(f"hub.api.{op}", req, timeout=5)
        return json.loads(reply.data)

    await nc.subscribe(inbox_subject, cb=inbox.on_message)
    print(f"[{log}] subscribed to {inbox_subject} (oneshot + sessions)")

    if cfg.broadcast_channel:
        await nc.subscribe(f"channel.{cfg.broadcast_channel}", cb=inbox.on_message)

    async def announce() -> None:
        """Heartbeat + (idempotent, upserting) registration. Re-registering on
        every beat means a worker that started before hub-server was listening
        (or survived a router restart) still shows up in `hub-agents`."""
        payload = {"identity": cfg.identity, **cfg.extra_heartbeat}
        await nc.publish(
            "hub.presence",
            make_envelope(cfg.identity, None, "hub.presence", "status", payload),
        )
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
        await nc.flush()

    async def heartbeat_loop() -> None:
        # Fast re-announce while the hub may still be starting, then steady.
        delays = [d for d in ANNOUNCE_BACKOFF if d < cfg.heartbeat_secs]
        while True:
            await asyncio.sleep(delays.pop(0) if delays else cfg.heartbeat_secs)
            try:
                await announce()
            except Exception as e:  # noqa: BLE001 - keep beating
                print(f"[{log}] heartbeat failed: {e}")

    # Graceful stop must be in place before anyone can see us (and signal us).
    current = asyncio.current_task()
    if current is not None:
        install_stop_signals(current)

    heartbeat_task: asyncio.Task | None = None
    dispatcher = asyncio.create_task(inbox.run())
    try:
        # Announce ourselves right after subscribing, so `hub-agents` shows
        # the worker immediately instead of after the first heartbeat interval.
        await announce()
        heartbeat_task = asyncio.create_task(heartbeat_loop())
        if cfg.session_resume:
            await session_resume.resume_all(api_request, cfg.identity, resume_session)
        print(f"[{log}] ready")

        while True:
            await asyncio.sleep(1)
    except (KeyboardInterrupt, asyncio.CancelledError):
        pass
    finally:
        if heartbeat_task is not None:
            heartbeat_task.cancel()
        dispatcher.cancel()  # cancels the running turn too (kills CLI groups)
        await asyncio.gather(dispatcher, return_exceptions=True)
        await nc.close()