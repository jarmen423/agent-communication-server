"""nats-hub MCP — session tool handlers (split out of ``hub_handlers``).

``start_session`` and ``send_to_session`` return the ``message_id`` of the
turn they sent. That id is the turn's task id for the cancel contract
(refocus-iteration-2.md §4.2): ``cancel_task(message_id)`` DMs the session's
worker a cancel and waits for that turn's terminal result on the session
channel.
"""

from __future__ import annotations

import uuid
from typing import Any

import hub_connection as conn
from hub_buffers import SessionTurn, hub
from hub_primitives import summarize_env


def _err(msg: str) -> dict:
    return {"ok": False, "error": msg}


def _from_guard(args: dict) -> dict | None:
    bad = conn.check_from_arg(args)
    return _err(bad) if bad else None


async def _start_session(args: dict) -> dict:
    if (bad := _from_guard(args)) is not None:
        return bad
    h = hub()
    await h.ensure_started()
    worker = args["worker"]
    prompt = args["prompt"]
    session_id = uuid.uuid4().hex[:8]
    session_channel = f"session.{session_id}"

    # Buffer replies before sending session_start so nothing is missed.
    await h.session_buffer(session_id)

    payload: dict[str, Any] = {
        "action": "session_start",
        "session_id": session_id,
        "session_channel": session_channel,
        "prompt": prompt,
    }
    for k in ("model", "provider", "cwd"):
        if args.get(k):
            payload[k] = args[k]

    env = conn.envelope(session_channel, payload, to=worker)
    h.session_workers[session_id] = worker
    h.track_turn(SessionTurn(env["meta"]["id"], session_id, worker, session_channel))
    await conn.publish(session_channel, env)

    resp = await conn.api_request("session.create", {
        "session_id": session_id,
        "orchestrator": conn.identity(),
        "worker": worker,
        "status": "active",
        "cwd": args.get("cwd"),
        "model": args.get("model"),
        "provider": args.get("provider"),
        "created_at": env["meta"]["timestamp"],
        "updated_at": env["meta"]["timestamp"],
        "metadata": {"prompt": prompt, "timeout": args.get("timeout", 30)},
    })
    if not resp.get("ok"):
        return {"ok": False, "error":
                f"session_start sent to {worker}, but session.create failed: "
                f"{resp.get('error')}"}

    return {"ok": True, "data": {"session_id": session_id, "channel": session_channel,
                                 "message_id": env["meta"]["id"]}}


async def _send_to_session(args: dict) -> dict:
    if (bad := _from_guard(args)) is not None:
        return bad
    h = hub()
    session_id = args["session_id"]
    channel = f"session.{session_id}"
    # Buffer the session channel first, so this turn's result is captured
    # (cancel_task / session_replies read it from there).
    await h.session_buffer(session_id)
    env = conn.envelope(
        channel,
        {"action": "session_send", "session_id": session_id, "message": args["message"]},
        to=None,
    )
    message_id = env["meta"]["id"]
    worker = await h.session_worker(session_id)
    if worker:
        h.track_turn(SessionTurn(message_id, session_id, worker, channel))
    # Broadcast on the session channel: the worker is subscribed once the
    # session is active (worker_runtime).
    await conn.publish(channel, env)
    data: dict[str, Any] = {"session_id": session_id, "message_id": message_id}
    if not worker:
        data["note"] = ("session worker unknown (no session record): this turn "
                        "can't be cancelled with cancel_task")
    return {"ok": True, "data": data}


async def _close_session(args: dict) -> dict:
    if (bad := _from_guard(args)) is not None:
        return bad
    session_id = args["session_id"]
    channel = f"session.{session_id}"
    env = conn.envelope(
        channel,
        {"action": "session_close", "session_id": session_id},
    )
    await conn.publish(channel, env)
    await hub().drop_session(session_id)
    resp = await conn.api_request("session.update_status",
                                  {"session_id": session_id, "status": "closed"})
    if not resp.get("ok"):
        return {"ok": False, "error": f"close sent, but status update failed: {resp.get('error')}"}
    return {"ok": True, "data": {"session_id": session_id, "status": "closed"}}


async def _session_replies(args: dict) -> dict:
    h = hub()
    session_id = args["session_id"]
    ch = await h.session_buffer(session_id)
    since = int(args.get("since_seq") or 0)
    limit = int(args.get("limit") or 50)
    items = ch.buf.since(since) if since else ch.buf.tail(limit)
    return {"ok": True, "data": {
        "session_id": session_id,
        "last_seq": ch.buf.last_seq,
        "messages": [summarize_env(it) for it in items[:limit]],
    }}


SESSION_HANDLERS = {
    "start_session": _start_session,
    "send_to_session": _send_to_session,
    "close_session": _close_session,
    "session_replies": _session_replies,
}
