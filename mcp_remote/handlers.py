"""Tool handlers of the remote MCP endpoint.

Shared tools run the stdio server's handlers unchanged (``hub_handlers``,
``hub_queries``, ``hub_sessions``): inside ``hub_connection.bound`` they
publish and query as the token's identity. The remote-only handlers below
keep no state between calls; they read results and inboxes back from
JetStream (``stored.py``). Every call is validated against the tool's strict
schema (``hub_validate``) and every result is size-capped.
"""

from __future__ import annotations

import contextvars
import json
from typing import Any

import hub_connection as conn
import hub_delivery
import hub_sessions
from hub_handlers import HANDLERS as STDIO_HANDLERS
from hub_validate import validated_handlers

from . import stored
from .paging import page_text
from .tools import SHARED, TOOLS

MAX_RESULT_CHARS = 120_000  # whole tool result; claude.ai caps at ~150k

# The verified access token of the current call (set by app.bind).
current_token: contextvars.ContextVar[Any] = contextvars.ContextVar("mcp_remote_token")


def _ok(data: dict) -> dict:
    return {"ok": True, "data": data}


async def _whoami(args: dict) -> dict:
    tok = current_token.get(None)
    return _ok({
        "identity": conn.identity(),
        "tenant": getattr(tok, "tenant", None),
        "client_id": getattr(tok, "client_id", None),
        "note": "this connector's identity comes from its access token; it is "
                "stamped as meta.from on every message",
    })


async def _delegate(args: dict) -> dict:
    nc = await conn.get_nc()
    worker = args["to"]
    env = conn.envelope("task", {}, kind="message", to=worker)
    task_id = env["meta"]["id"]
    channel = hub_delivery.task_channel_for(task_id)
    # Refuse before sending if results could never be read back.
    await stored.read_stored(nc, f"channel.{channel}", limit=1)
    env["meta"]["channel"] = channel
    env["payload"] = {"prompt": args["prompt"], "task_channel": channel}
    await conn.publish(channel, env)
    return _ok({"task_id": task_id, "worker": worker, "task_channel": channel,
                "state": "submitted",
                "next": "call get_task_result with this task_id and worker"})


def _view_data(view: stored.TaskView) -> dict:
    return {"task_id": view.task_id, "worker": view.worker,
            "task_channel": view.channel, "state": view.state,
            "last_status": view.last_status}


async def _get_task_result(args: dict) -> dict:
    nc = await conn.get_nc()
    view = stored.TaskView(args["task_id"], args["worker"])
    cursor = args.get("cursor")
    if cursor is None:
        await view.wait(nc, float(args.get("wait_secs", 25)))
    else:
        await view.update(nc)
    data = _view_data(view)
    if view.result is None:
        if cursor is not None:
            return {"ok": False, "error": "no result yet for this task; a cursor "
                    "only pages a finished result"}
        data["recent_events"] = view.events[-5:]
        data["note"] = ("still running: call get_task_result again (now or in a "
                        "later turn), or cancel_task")
        return _ok(data)
    payload = view.result.get("payload") or {}
    result = payload.get("result")
    text = result if isinstance(result, str) else json.dumps(result, ensure_ascii=False)
    page = page_text(text or "", int(cursor or 0), int(args.get("max_chars", 100_000)))
    data.update({"status": payload.get("status"), "error": payload.get("error"),
                 "result": page.pop("text"), "result_page": page})
    return _ok(data)


async def _cancel_task(args: dict) -> dict:
    nc = await conn.get_nc()
    view = stored.TaskView(args["task_id"], args["worker"])
    await view.update(nc)
    if view.result is not None:
        return _ok({**_view_data(view), "note": f"task already finished "
                                                f"({view.state}); cancel not sent"})
    env = conn.envelope(view.channel, {"action": "cancel", "task_id": view.task_id},
                        kind="control", to=view.worker)
    await conn.publish(view.channel, env)
    await view.wait(nc, float(args.get("wait_secs", 5)))
    data = {**_view_data(view), "cancel_sent": True}
    if view.result is None:
        data["note"] = ("cancel sent; the worker has not confirmed yet. "
                        "get_task_result shows when it does (state cancelled)")
    elif view.state != "cancelled":
        data["note"] = f"task finished ({view.state}) before the cancel took effect"
    return _ok(data)


async def _read_inbox(args: dict) -> dict:
    nc = await conn.get_nc()
    out = await stored.read_inbox(nc, conn.identity(), limit=int(args.get("limit", 20)),
                                  wait_secs=float(args.get("wait_secs", 0)),
                                  max_chars=100_000)
    return _ok({"identity": conn.identity(), **out})


async def _start_session(args: dict) -> dict:
    nc = await conn.get_nc()
    session_id, channel, env = hub_sessions.session_start_envelope(args)
    await stored.read_stored(nc, f"channel.{channel}", limit=1)  # durable hub?
    await conn.publish(channel, env)
    if (failed := await hub_sessions.create_session_record(session_id, args, env)) is not None:
        return failed
    return _ok({"session_id": session_id, "channel": channel, "worker": args["worker"],
                "message_id": env["meta"]["id"],
                "next": "read replies with session_replies(session_id)"})


async def _send_to_session(args: dict) -> dict:
    session_id = args["session_id"]
    env = hub_sessions.session_send_envelope(session_id, args["message"])
    await conn.publish(f"session.{session_id}", env)
    return _ok({"session_id": session_id, "message_id": env["meta"]["id"]})


async def _session_replies(args: dict) -> dict:
    nc = await conn.get_nc()
    out = await stored.read_session(
        nc, args["session_id"], after_seq=int(args.get("since_seq", 0)),
        limit=int(args.get("limit", 50)), wait_secs=float(args.get("wait_secs", 0)))
    return _ok({"session_id": args["session_id"], **out})


async def _close_session(args: dict) -> dict:
    return await hub_sessions.close_session(args["session_id"])


def _capped(name: str, handler):
    async def run(args: dict) -> dict:
        result = await handler(args)
        size = len(json.dumps(result, ensure_ascii=False, default=str))
        if size > MAX_RESULT_CHARS:
            return {"ok": False, "error": (
                f"{name}: result is {size} characters, over this endpoint's "
                f"{MAX_RESULT_CHARS} limit; narrow the request (e.g. a smaller limit)")}
        return result

    return run


_RAW = {
    **{name: STDIO_HANDLERS[name] for name in SHARED if name != "close_session"},
    "close_session": _close_session,
    "whoami": _whoami,
    "delegate": _delegate,
    "get_task_result": _get_task_result,
    "cancel_task": _cancel_task,
    "read_inbox": _read_inbox,
    "start_session": _start_session,
    "send_to_session": _send_to_session,
    "session_replies": _session_replies,
}

HANDLERS = {name: _capped(name, h) for name, h in validated_handlers(TOOLS, _RAW).items()}
