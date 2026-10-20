"""nats-hub MCP — read-only query-API handlers.

Thin passthroughs onto ``hub.api.*`` request-reply ops (history, threads,
session/wave listings, analytics). Split from ``hub_handlers`` for the
file-size cap; merged into ``HANDLERS`` there.
"""

from __future__ import annotations

from typing import Any

import hub_connection as conn


def _err(msg: str) -> dict:
    return {"ok": False, "error": msg}


async def _list_sessions(args: dict) -> dict:
    params: dict[str, Any] = {}
    for k in ("status", "worker", "orchestrator", "limit"):
        if args.get(k):
            params[k] = args[k]
    return await conn.api_request("session.list", params)


async def _get_session(args: dict) -> dict:
    return await conn.api_request("session.get", {"session_id": args["session_id"]})


async def _get_history(args: dict) -> dict:
    params: dict[str, Any] = {}
    for k in ("channel", "from", "to", "kind"):
        if args.get(k):
            params[k] = args[k]
    for k in ("since", "until", "limit"):
        if args.get(k) is not None:
            params[k] = args[k]
    return await conn.api_request("history.query", params)


async def _get_thread(args: dict) -> dict:
    return await conn.api_request("thread.get", {"root_id": args["root_id"]})


async def _list_pending(args: dict) -> dict:
    return await conn.api_request("thread.pending", {"identity": args["identity"]})


async def _list_waves(args: dict) -> dict:
    params = {"status": args["status"]} if args.get("status") else {}
    return await conn.api_request("wave.list", params)


async def _get_wave(args: dict) -> dict:
    return await conn.api_request("wave.get", {"wave_id": args["wave_id"]})


async def _list_wave_tasks(args: dict) -> dict:
    return await conn.api_request("wave.list_tasks", {"wave_id": args["wave_id"]})


async def _get_wave_task(args: dict) -> dict:
    return await conn.api_request("wave.get_task", {
        "wave_id": args["wave_id"], "task_id": args["task_id"],
    })


async def _wave_status(args: dict) -> dict:
    return await conn.api_request("wave.status", {"wave_id": args["wave_id"]})


async def _get_analytics(args: dict) -> dict:
    op_map = {
        "message_rate": "stats.message_rate",
        "latency": "stats.latency",
        "agent_activity": "stats.agent_activity",
        "channel_hotspots": "stats.channel_hotspots",
        "error_rate": "stats.error_rate",
    }
    op = op_map.get(args["metric"])
    if not op:
        return _err(f"unknown metric: {args['metric']}")

    params: dict[str, Any] = {"secs": args.get("secs", 3600)}
    if args["metric"] in ("message_rate", "error_rate"):
        params["interval"] = args.get("interval", "hour")
    if args["metric"] == "latency" and args.get("channel"):
        params["channel"] = args["channel"]
    if args["metric"] == "agent_activity":
        params["identity"] = args.get("identity", "")
    if args["metric"] == "channel_hotspots":
        params["limit"] = args.get("limit", 10)

    return await conn.api_request(op, params)


QUERY_HANDLERS = {
    "list_sessions": _list_sessions,
    "get_session": _get_session,
    "get_history": _get_history,
    "get_thread": _get_thread,
    "list_pending": _list_pending,
    "list_waves": _list_waves,
    "get_wave": _get_wave,
    "list_wave_tasks": _list_wave_tasks,
    "get_wave_task": _get_wave_task,
    "wave_status": _wave_status,
    "get_analytics": _get_analytics,
}
