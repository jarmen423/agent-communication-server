"""nats-hub MCP — tool handlers.

Every handler takes the tool's ``arguments`` dict and returns
``{"ok": True, "data": ...}`` or ``{"ok": False, "error": ...}``. Identity
comes from ``NATS_HUB_IDENTITY`` via ``hub_connection.identity()``; a stale
``from``/``orchestrator`` argument is rejected unless it matches.
"""

from __future__ import annotations

import uuid
from typing import Any

import hub_connection as conn
from hub_buffers import hub
from hub_primitives import summarize_env as _summarize_env
from hub_providers import check_providers as _check_providers
from hub_queries import QUERY_HANDLERS
from hub_sessions import SESSION_HANDLERS


def _err(msg: str) -> dict:
    return {"ok": False, "error": msg}


def _from_guard(args: dict) -> dict | None:
    bad = conn.check_from_arg(args)
    return _err(bad) if bad else None


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
                    + (f" from {sender}" if sender else "")
                    + f" (inbox last_seq={h.inbox.buf.last_seq}); call wait_for_message again "
                    "to keep waiting, or read_inbox to see buffered messages")
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
        return _err(f"task {tracker.task_id} was evicted before its result "
                    "arrived (too many tracked tasks); check get_history("
                    f"channel='{tracker.task_channel}')")
    if result.get("state") == "timeout":
        # Don't leave the worker running a task nobody waits for any more.
        try:
            await hub().send_cancel(tracker)
            sent = "a cancel was sent to the worker"
        except Exception as e:  # noqa: BLE001 - bus down
            sent = f"sending the cancel failed ({e or type(e).__name__})"
        return _err(f"delegate timed out after {timeout}s "
                    f"(task_id={tracker.task_id}); {sent}. task_status / "
                    "wait_for_task show whether it confirmed (state cancelled)")
    if result.get("state") == "evicted":
        return _err(_evicted(tracker.task_id, tracker.task_channel))
    payload = (result.get("result") or {})
    if result.get("state") == "error":
        return _err(f"task failed: {payload.get('error', 'unknown')}")
    if result.get("state") == "cancelled":
        return _err(f"task {tracker.task_id} was cancelled before it finished")
    return {"ok": True, "data": {
        "task_id": tracker.task_id,
        "task_channel": tracker.task_channel,
        "to": tracker.worker,
        "reply": payload,
    }}


def _evicted(task_id: str, task_channel: str) -> str:
    return (f"task {task_id} was evicted from tracking before its result "
            "arrived (too many tracked tasks); check get_history("
            f"channel='{task_channel}')")


def _unknown_task(task_id: str) -> dict:
    return _err(
        f"unknown task_id {task_id!r}: this MCP server only tracks tasks it "
        "delegated itself (delegate_async/delegate_task) since it started, "
        "and evicts the oldest finished ones. Use the task_id returned by "
        "delegate_async, or get_history(channel='task.<id>') for older tasks.")


async def _task_status(args: dict) -> dict:
    snap = await hub().task_status(args["task_id"], int(args.get("events_tail", 10)))
    if snap is None:
        return _unknown_task(args["task_id"])
    return {"ok": True, "data": snap}


async def _wait_for_task(args: dict) -> dict:
    timeout = float(args["timeout"])
    result = await hub().wait_for_task(args["task_id"], timeout)
    if result is None:
        return _unknown_task(args["task_id"])
    if result.get("state") == "evicted":
        return _err(_evicted(args["task_id"], result.get("task_channel", "task.<id>")))
    if result.get("state") == "timeout":
        return _err(f"no result within {timeout}s "
                    f"(last_status={result.get('last_status')}); call "
                    "wait_for_task again to keep waiting, or cancel_task")
    return {"ok": True, "data": result}


async def _cancel_task(args: dict) -> dict:
    """Contract §4.2: returns the terminal snapshot (state `cancelled`, or
    done/error if the task finished before the cancel took effect)."""
    task_id = args["task_id"]
    timeout = float(args.get("timeout", 10))
    res = await hub().cancel_task(task_id, timeout)
    if res is None:
        return _unknown_task(task_id)
    snap = res["snapshot"]
    if res["timed_out"]:
        return {"ok": False, "data": snap, "error": (
            f"cancel sent to {snap['worker']}, but no terminal result within "
            f"{timeout:g}s. The worker may not support cancel yet or is busy; "
            "the task is still tracked — wait_for_task or cancel_task again.")}
    if not res["cancel_sent"]:
        snap = {**snap, "note": f"task already finished ({snap['state']}); "
                                "cancel not sent"}
    elif snap["state"] != "cancelled":
        snap = {**snap, "note": f"task finished ({snap['state']}) before the "
                                "cancel took effect"}
    return {"ok": True, "data": snap}


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
    "cancel_task": _cancel_task,
    **SESSION_HANDLERS,
    "create_wave": _create_wave,
    "spawn_wave": _spawn_wave,
    "cancel_wave": _cancel_wave,
    **QUERY_HANDLERS,
}
