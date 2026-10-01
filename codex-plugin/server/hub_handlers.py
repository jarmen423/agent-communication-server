"""nats-hub MCP — tool handlers.

Every handler takes the tool's ``arguments`` dict and returns
``{"ok": True, "data": ...}`` or ``{"ok": False, "error": ...}``. Identity
comes from ``NATS_HUB_IDENTITY`` via ``hub_connection.identity()``; a stale
``from``/``orchestrator`` argument is rejected unless it matches.
"""

from __future__ import annotations

import json
import uuid
from typing import Any

import hub_connection as conn
from hub_buffers import hub
from hub_queries import QUERY_HANDLERS


def _err(msg: str) -> dict:
    return {"ok": False, "error": msg}


def _from_guard(args: dict) -> dict | None:
    bad = conn.check_from_arg(args)
    return _err(bad) if bad else None


def _summarize_env(it: dict) -> dict:
    env = it["env"]
    return {
        "seq": it["seq"],
        "id": env.get("meta", {}).get("id"),
        "from": env.get("meta", {}).get("from"),
        "to": env.get("meta", {}).get("to"),
        "channel": env.get("meta", {}).get("channel"),
        "kind": env.get("meta", {}).get("kind"),
        "timestamp": env.get("meta", {}).get("timestamp"),
        "reply_to": env.get("meta", {}).get("reply_to"),
        "payload": env.get("payload"),
    }


# ── Discovery ────────────────────────────────────────────────────


async def _list_agents(args: dict) -> dict:
    # AgentFilter.capabilities is a required field (Vec, no serde default).
    params: dict[str, Any] = {"capabilities": args.get("capability") or []}
    if args.get("alive_within_secs"):
        params["alive_within_secs"] = args["alive_within_secs"]
    if args.get("limit"):
        params["limit"] = args["limit"]
    return await conn.api_request("agent.find", params)


async def _get_agent(args: dict) -> dict:
    return await conn.api_request("agent.get", {"identity": args["identity"]})


async def _check_providers(args: dict) -> dict:
    """Honest provider check: bus liveness + optional supervisor probes."""
    alive_secs = int(args.get("alive_within_secs") or 120)
    found = await conn.api_request(
        "agent.find", {"capabilities": [], "alive_within_secs": alive_secs})
    if not found.get("ok"):
        return found
    agents = found.get("data", {}).get("agents") or []
    alive = [
        {
            "identity": a.get("identity"),
            "capabilities": a.get("capabilities", []),
            "last_seen": a.get("last_seen"),
            "models": (a.get("metadata") or {}).get("models"),
        }
        for a in agents
    ]

    probes = []
    providers = args.get("providers") or []
    if providers:
        nc = await conn.get_nc()
        for provider in providers:
            try:
                req = json.dumps({"provider": provider}).encode()
                reply = await nc.request("hub.worker.models", req, timeout=5)
                resp = json.loads(reply.data)
                probes.append({
                    "provider": provider,
                    "ok": bool(resp.get("ok")),
                    "models": resp.get("models") or [],
                    "error": resp.get("error"),
                })
            except Exception as e:
                probes.append({"provider": provider, "ok": False, "error": str(e)})

    return {"ok": True, "data": {
        "alive_agents": alive,
        "provider_probes": probes,
        "verifies": "bus registration + heartbeat liveness + supervisor "
                    "model-list responses",
        "does_not_verify": "provider credentials, CLI health, or whether a "
                           "worker will actually accept a task",
    }}


# ── Messaging ────────────────────────────────────────────────────


async def _send_message(args: dict) -> dict:
    if (bad := _from_guard(args)) is not None:
        return bad
    env = conn.envelope(args["channel"], {"message": args["message"]})
    await conn.publish(args["channel"], env)
    return {"ok": True, "data": {"message_id": env["meta"]["id"], "channel": args["channel"]}}


async def _send_direct(args: dict) -> dict:
    if (bad := _from_guard(args)) is not None:
        return bad
    channel = args.get("channel", "dm")
    env = conn.envelope(channel, {"message": args["message"]}, to=args["to"])
    await conn.publish(channel, env)
    return {"ok": True, "data": {"message_id": env["meta"]["id"], "to": args["to"]}}


async def _send_status(args: dict) -> dict:
    if (bad := _from_guard(args)) is not None:
        return bad
    env = conn.envelope(args["channel"], {"status": args["status"]}, kind="status")
    await conn.publish(args["channel"], env)
    return {"ok": True, "data": {"message_id": env["meta"]["id"]}}


async def _read_inbox(args: dict) -> dict:
    h = hub()
    await h.ensure_started()
    since = int(args.get("since_seq") or 0)
    limit = int(args.get("limit") or 50)
    items = h.inbox.buf.since(since) if since else h.inbox.buf.tail(limit)
    if since:
        items = items[:limit]
    return {"ok": True, "data": {
        "identity": conn.identity(),
        "last_seq": h.inbox.buf.last_seq,
        "messages": [_summarize_env(it) for it in items],
    }}


async def _wait_for_message(args: dict) -> dict:
    h = hub()
    await h.ensure_started()
    timeout = float(args["timeout"])
    # Default: only messages arriving after this call (a "wait" shouldn't
    # return stale inbox backlog). Pass since_seq explicitly to replay.
    since_arg = args.get("since_seq")
    since = int(since_arg) if since_arg is not None else h.inbox.buf.last_seq
    sender = args.get("from")
    it = await h.inbox.buf.wait_for(
        lambda env: sender is None or env.get("meta", {}).get("from") == sender,
        since=since,
        timeout=timeout,
    )
    if it is None:
        return _err(f"no inbox message within {timeout}s"
                    + (f" from {sender}" if sender else ""))
    return {"ok": True, "data": _summarize_env(it)}


# ── Task delegation ──────────────────────────────────────────────


async def _delegate_async(args: dict) -> dict:
    if (bad := _from_guard(args)) is not None:
        return bad
    tracker = await hub().delegate_async(args["to"], args["prompt"])
    return {"ok": True, "data": {
        "task_id": tracker.task_id,
        "task_channel": tracker.task_channel,
        "to": tracker.worker,
    }}


async def _delegate_task(args: dict) -> dict:
    if (bad := _from_guard(args)) is not None:
        return bad
    timeout = float(args.get("timeout", 120))
    tracker = await hub().delegate_async(args["to"], args["prompt"])
    result = await hub().wait_for_task(tracker.task_id, timeout)
    if result is None:
        return _err("task tracker missing")
    if result.get("state") == "timeout":
        return _err(f"delegate timed out after {timeout}s "
                    f"(task_id={tracker.task_id})")
    payload = (result.get("result") or {})
    if result.get("state") == "error":
        return _err(f"task failed: {payload.get('error', 'unknown')}")
    return {"ok": True, "data": {
        "task_id": tracker.task_id,
        "task_channel": tracker.task_channel,
        "to": tracker.worker,
        "reply": payload,
    }}


async def _task_status(args: dict) -> dict:
    snap = await hub().task_status(args["task_id"], int(args.get("events_tail", 10)))
    if snap is None:
        return _err(f"unknown task_id: {args['task_id']}")
    return {"ok": True, "data": snap}


async def _wait_for_task(args: dict) -> dict:
    timeout = float(args["timeout"])
    result = await hub().wait_for_task(args["task_id"], timeout)
    if result is None:
        return _err(f"unknown task_id: {args['task_id']}")
    if result.get("state") == "timeout":
        return _err(f"no result within {timeout}s "
                    f"(last_status={result.get('last_status')})")
    return {"ok": True, "data": result}


# ── Sessions ─────────────────────────────────────────────────────


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

    return {"ok": True, "data": {"session_id": session_id, "channel": session_channel}}


async def _send_to_session(args: dict) -> dict:
    if (bad := _from_guard(args)) is not None:
        return bad
    session_id = args["session_id"]
    channel = f"session.{session_id}"
    env = conn.envelope(
        channel,
        {"action": "session_send", "session_id": session_id, "message": args["message"]},
        to=None,
    )
    # Broadcast on the session channel: the worker is subscribed once the
    # session is active (worker_runtime).
    await conn.publish(channel, env)
    return {"ok": True, "data": {"session_id": session_id}}


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
        "messages": [_summarize_env(it) for it in items[:limit]],
    }}


# ── Waves ────────────────────────────────────────────────────────


async def _create_wave(args: dict) -> dict:
    if (bad := _from_guard(args)) is not None:
        return bad
    wave_id = uuid.uuid4().hex[:8]
    wave = {
        "wave_id": wave_id,
        "goal": args["goal"],
        "status": "pending",
        "orchestrator": conn.identity(),
        "created_at": conn.now(),
    }
    tasks: list[dict[str, Any]] = []
    for t in args["tasks"]:
        tasks.append({
            "task_id": t.get("task_id") or uuid.uuid4().hex[:8],
            "worker": t["worker"],
            "goal": t["goal"],
            "write_scope": t.get("write_scope") or [],
            "dependencies": t.get("dependencies") or [],
            "handoff_path": t.get("handoff_path"),
            "verify_cmd": t.get("verify_cmd"),
        })
    # One atomic op: server-side validation (incl. dependency cycles)
    # runs before anything is persisted.
    result = await conn.api_request(
        "wave.create", {"wave": wave, "tasks": tasks})
    if not result.get("ok"):
        return result
    result["data"]["orchestration"] = "hub-server"
    return result


async def _spawn_wave(args: dict) -> dict:
    """Hand the wave to the hub-server orchestrator and return.

    The server dispatches ready tasks, enforces worker liveness, and drives
    the wave to a terminal status — this MCP server does not need to stay
    alive for the wave to finish.
    """
    if (bad := _from_guard(args)) is not None:
        return bad
    return await conn.api_request("wave.spawn", {
        "wave_id": args["wave_id"],
        "timeout_secs": int(args.get("timeout", 3600)),
    })


async def _cancel_wave(args: dict) -> dict:
    """Cancel a wave: non-terminal tasks → cancelled; running workers get
    the §4.2 cancel DM. Errors surface from the orchestrator."""
    if (bad := _from_guard(args)) is not None:
        return bad
    return await conn.api_request("wave.cancel", {"wave_id": args["wave_id"]})


HANDLERS = {
    "list_agents": _list_agents,
    "get_agent": _get_agent,
    "check_providers": _check_providers,
    "send_message": _send_message,
    "send_direct": _send_direct,
    "send_status": _send_status,
    "read_inbox": _read_inbox,
    "wait_for_message": _wait_for_message,
    "delegate_async": _delegate_async,
    "delegate_task": _delegate_task,
    "task_status": _task_status,
    "wait_for_task": _wait_for_task,
    "start_session": _start_session,
    "send_to_session": _send_to_session,
    "close_session": _close_session,
    "session_replies": _session_replies,
    "create_wave": _create_wave,
    "spawn_wave": _spawn_wave,
    "cancel_wave": _cancel_wave,
    **QUERY_HANDLERS,
}
