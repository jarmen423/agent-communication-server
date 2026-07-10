#!/usr/bin/env python3
"""nats-hub MCP server — exposes hub tools to any MCP-compatible agent.

Tools exposed:
  - list_agents:        List/search registered agents (by capability, alive window)
  - get_agent:          Get a single agent by identity
  - send_message:       Broadcast a message on a channel
  - send_direct:        Direct-message a specific agent (DM via inbox routing)
  - start_session:      Start a stateful session with a worker agent
  - send_to_session:    Send a follow-up message on an existing session
  - close_session:      Close a session
  - list_sessions:      List sessions (filter by status/worker/orchestrator)
  - get_session:        Get a single session by ID
  - delegate_task:      Delegate a task to a worker and wait for the reply
  - get_history:        Query message history (by channel/from/to/kind/time range)
  - get_thread:         Get a conversation thread by root message ID
  - list_pending:       List unanswered messages for an agent
  - send_status:        Send a status update on a channel
  - create_wave:        Create a wave (parallel task group with disjoint write scopes)
  - list_waves:         List waves (optionally filter by status)
  - get_wave:           Get a wave by ID
  - list_wave_tasks:    List tasks in a wave
  - get_wave_task:      Get a single wave task
  - get_analytics:      Get activity stats (message rate, latency, channel hotspots)

All operations go through NATS (messaging) or the hub-server query API
(DB reads/writes via request-reply on hub.api.<op>).  No direct DB access.
"""

import asyncio
import json
import os
import sys
import time
import uuid
from datetime import datetime, timezone

import nats
from mcp.server import Server
from mcp.server.stdio import stdio_server
from mcp.types import (
    CallToolResult,
    TextContent,
    Tool,
)

# ── Config ────────────────────────────────────────────────────────

NATS_URL = os.environ.get("NATS_URL", "nats://127.0.0.1:4222")
HUB_IDENTITY = os.environ.get("NATS_HUB_IDENTITY", "mcp-server")
API_TIMEOUT = float(os.environ.get("NATS_HUB_API_TIMEOUT", "10"))

# ── NATS connection (lazily initialised) ──────────────────────────

_nc: nats.NATS | None = None


async def get_nc() -> nats.NATS:
    global _nc
    if _nc is None or _nc.is_closed:
        _nc = await nats.connect(NATS_URL, name=HUB_IDENTITY)
    return _nc


# ── Helpers ───────────────────────────────────────────────────────


def _ts() -> str:
    return datetime.now(timezone.utc).isoformat()


def _envelope(
    channel: str,
    payload: dict,
    from_id: str,
    kind: str = "message",
    to: str | None = None,
    reply_to: str | None = None,
) -> dict:
    """Build a nats-hub wire envelope."""
    env = {
        "meta": {
            "id": str(uuid.uuid4()),
            "from": from_id,
            "channel": channel,
            "to": to,
            "timestamp": _ts(),
            "kind": kind,
            "reply_to": reply_to,
        },
        "payload": payload,
    }
    return env


async def _publish_envelope(channel: str, env: dict) -> None:
    """Publish an envelope to hub.send.<channel>."""
    nc = await get_nc()
    subject = f"hub.send.{channel}"
    await nc.publish(subject, json.dumps(env).encode())
    await nc.flush()


async def _api_request(op: str, params: dict) -> dict:
    """Send a query-API request to hub.api.<op> and return the response data."""
    nc = await get_nc()
    subject = f"hub.api.{op}"
    req = json.dumps({"op": op, "params": params}).encode()
    try:
        reply = await nc.request(subject, req, timeout=API_TIMEOUT)
    except Exception as e:
        return {"ok": False, "error": f"query API '{op}' failed: {e}"}
    resp = json.loads(reply.data)
    if not resp.get("ok"):
        return {"ok": False, "error": resp.get("error", "unknown error")}
    return {"ok": True, "data": resp.get("data")}


# ── Tool definitions ──────────────────────────────────────────────

TOOLS = [
    Tool(
        name="list_agents",
        description=(
            "List agents registered on the nats-hub bus. Optionally filter by "
            "capabilities or liveness window. Returns identity, capabilities, "
            "and last_seen for each agent."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "capability": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Only return agents with ALL these capabilities",
                },
                "alive_within_secs": {
                    "type": "integer",
                    "description": "Only return agents seen within this many seconds",
                },
                "limit": {
                    "type": "integer",
                    "description": "Max number of results",
                },
            },
        },
    ),
    Tool(
        name="get_agent",
        description="Get details for a single agent by identity.",
        inputSchema={
            "type": "object",
            "required": ["identity"],
            "properties": {
                "identity": {"type": "string", "description": "Agent identity string"},
            },
        },
    ),
    Tool(
        name="send_message",
        description=(
            "Broadcast a message on a channel. All subscribers on that channel "
            "will receive it. The sender identity is stamped on the envelope."
        ),
        inputSchema={
            "type": "object",
            "required": ["channel", "message", "from"],
            "properties": {
                "channel": {"type": "string", "description": "Channel name (e.g. 'agents.broadcast')"},
                "message": {"type": "string", "description": "Message text to send"},
                "from": {"type": "string", "description": "Sender identity"},
            },
        },
    ),
    Tool(
        name="send_direct",
        description=(
            "Send a direct message (DM) to a specific agent via inbox routing. "
            "Only the recipient agent sees it."
        ),
        inputSchema={
            "type": "object",
            "required": ["to", "message", "from"],
            "properties": {
                "to": {"type": "string", "description": "Recipient agent identity"},
                "message": {"type": "string", "description": "Message text"},
                "from": {"type": "string", "description": "Sender identity"},
                "channel": {
                    "type": "string",
                    "description": "Channel context (default: 'dm')",
                    "default": "dm",
                },
            },
        },
    ),
    Tool(
        name="send_status",
        description="Send a status update on a channel (e.g. 'working', 'idle', 'ready').",
        inputSchema={
            "type": "object",
            "required": ["channel", "status", "from"],
            "properties": {
                "channel": {"type": "string"},
                "status": {"type": "string", "description": "Status text (e.g. 'working', 'idle')"},
                "from": {"type": "string"},
            },
        },
    ),
    Tool(
        name="start_session",
        description=(
            "Start a stateful session with a worker agent. Sends a session_start "
            "message to the worker's inbox and returns the session ID. Use "
            "send_to_session for follow-up messages and close_session when done."
        ),
        inputSchema={
            "type": "object",
            "required": ["worker", "from", "prompt"],
            "properties": {
                "worker": {"type": "string", "description": "Worker agent identity"},
                "from": {"type": "string", "description": "Orchestrator identity"},
                "prompt": {"type": "string", "description": "Initial prompt for the worker"},
                "model": {"type": "string", "description": "Model to use (optional)"},
                "provider": {"type": "string", "description": "Provider to use (optional)"},
                "cwd": {"type": "string", "description": "Working directory (optional)"},
                "timeout": {"type": "integer", "description": "Timeout in seconds", "default": 30},
            },
        },
    ),
    Tool(
        name="send_to_session",
        description="Send a follow-up message on an existing session channel.",
        inputSchema={
            "type": "object",
            "required": ["session_id", "message", "from"],
            "properties": {
                "session_id": {"type": "string"},
                "message": {"type": "string"},
                "from": {"type": "string"},
            },
        },
    ),
    Tool(
        name="close_session",
        description="Close a session (sends session_close on the session channel).",
        inputSchema={
            "type": "object",
            "required": ["session_id", "from"],
            "properties": {
                "session_id": {"type": "string"},
                "from": {"type": "string"},
            },
        },
    ),
    Tool(
        name="list_sessions",
        description="List sessions, optionally filtered by status, worker, or orchestrator.",
        inputSchema={
            "type": "object",
            "properties": {
                "status": {"type": "string", "description": "e.g. 'active', 'closed'"},
                "worker": {"type": "string"},
                "orchestrator": {"type": "string"},
                "limit": {"type": "integer"},
            },
        },
    ),
    Tool(
        name="get_session",
        description="Get a single session by ID.",
        inputSchema={
            "type": "object",
            "required": ["session_id"],
            "properties": {
                "session_id": {"type": "string"},
            },
        },
    ),
    Tool(
        name="delegate_task",
        description=(
            "Delegate a task to a worker agent and wait for the reply. "
            "Creates a task channel, sends the task to the worker's inbox, "
            "and waits up to timeout seconds for the response."
        ),
        inputSchema={
            "type": "object",
            "required": ["to", "prompt", "from"],
            "properties": {
                "to": {"type": "string", "description": "Worker agent identity"},
                "prompt": {"type": "string", "description": "Task/prompt to send"},
                "from": {"type": "string", "description": "Sender identity"},
                "timeout": {"type": "integer", "description": "Timeout in seconds", "default": 120},
            },
        },
    ),
    Tool(
        name="get_history",
        description=(
            "Query message history from the persistent store. Filter by channel, "
            "sender, recipient, message kind, or time range."
        ),
        inputSchema={
            "type": "object",
            "properties": {
                "channel": {"type": "string"},
                "from": {"type": "string", "description": "Sender identity"},
                "to": {"type": "string", "description": "Recipient identity"},
                "kind": {"type": "string", "description": "message|status|event|control|human"},
                "since": {"type": "string", "description": "ISO 8601 timestamp"},
                "until": {"type": "string", "description": "ISO 8601 timestamp"},
                "limit": {"type": "integer", "description": "Max results (most recent first)"},
            },
        },
    ),
    Tool(
        name="get_thread",
        description="Get a conversation thread by root message ID.",
        inputSchema={
            "type": "object",
            "required": ["root_id"],
            "properties": {
                "root_id": {"type": "string", "description": "Root envelope ID"},
            },
        },
    ),
    Tool(
        name="list_pending",
        description="List unanswered (pending) messages for an agent.",
        inputSchema={
            "type": "object",
            "required": ["identity"],
            "properties": {
                "identity": {"type": "string", "description": "Agent identity"},
            },
        },
    ),
    Tool(
        name="create_wave",
        description=(
            "Create a wave — a group of parallel tasks with disjoint write scopes. "
            "Provide a goal and a list of tasks. Returns the wave_id."
        ),
        inputSchema={
            "type": "object",
            "required": ["goal", "orchestrator", "tasks"],
            "properties": {
                "goal": {"type": "string", "description": "High-level goal for the wave"},
                "orchestrator": {"type": "string"},
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["worker", "goal"],
                        "properties": {
                            "worker": {"type": "string"},
                            "goal": {"type": "string"},
                        },
                    },
                    "description": "Tasks in this wave",
                },
            },
        },
    ),
    Tool(
        name="list_waves",
        description="List waves, optionally filtered by status.",
        inputSchema={
            "type": "object",
            "properties": {
                "status": {"type": "string", "description": "e.g. 'active', 'completed'"},
            },
        },
    ),
    Tool(
        name="get_wave",
        description="Get a wave by ID.",
        inputSchema={
            "type": "object",
            "required": ["wave_id"],
            "properties": {
                "wave_id": {"type": "string"},
            },
        },
    ),
    Tool(
        name="list_wave_tasks",
        description="List tasks in a wave.",
        inputSchema={
            "type": "object",
            "required": ["wave_id"],
            "properties": {
                "wave_id": {"type": "string"},
            },
        },
    ),
    Tool(
        name="get_wave_task",
        description="Get a single task within a wave.",
        inputSchema={
            "type": "object",
            "required": ["wave_id", "task_id"],
            "properties": {
                "wave_id": {"type": "string"},
                "task_id": {"type": "string"},
            },
        },
    ),
    Tool(
        name="get_analytics",
        description=(
            "Get analytics: message rate, latency stats, agent activity, "
            "channel hotspots, or error rate. Specify which metric via 'metric' param."
        ),
        inputSchema={
            "type": "object",
            "required": ["metric"],
            "properties": {
                "metric": {
                    "type": "string",
                    "enum": ["message_rate", "latency", "agent_activity", "channel_hotspots", "error_rate"],
                    "description": "Which analytics metric to fetch",
                },
                "secs": {"type": "integer", "description": "Time window in seconds (default 3600)", "default": 3600},
                "interval": {"type": "string", "description": "Bucket interval: minute|hour|day", "default": "hour"},
                "channel": {"type": "string", "description": "Filter by channel (latency only)"},
                "identity": {"type": "string", "description": "Agent identity (agent_activity only)"},
                "limit": {"type": "integer", "description": "Max results (channel_hotspots only)", "default": 10},
            },
        },
    ),
]


# ── Tool handlers ─────────────────────────────────────────────────


async def _list_agents(args: dict) -> dict:
    params = {}
    if args.get("capability"):
        params["capabilities"] = args["capability"]
    if args.get("alive_within_secs"):
        params["alive_within_secs"] = args["alive_within_secs"]
    if args.get("limit"):
        params["limit"] = args["limit"]
    return await _api_request("agent.find", params)


async def _get_agent(args: dict) -> dict:
    return await _api_request("agent.get", {"identity": args["identity"]})


async def _send_message(args: dict) -> dict:
    env = _envelope(
        channel=args["channel"],
        payload={"message": args["message"]},
        from_id=args["from"],
        kind="message",
    )
    await _publish_envelope(args["channel"], env)
    return {"ok": True, "data": {"message_id": env["meta"]["id"], "channel": args["channel"]}}


async def _send_direct(args: dict) -> dict:
    channel = args.get("channel", "dm")
    env = _envelope(
        channel=channel,
        payload={"message": args["message"]},
        from_id=args["from"],
        kind="message",
        to=args["to"],
    )
    await _publish_envelope(channel, env)
    return {"ok": True, "data": {"message_id": env["meta"]["id"], "to": args["to"]}}


async def _send_status(args: dict) -> dict:
    env = _envelope(
        channel=args["channel"],
        payload={"status": args["status"]},
        from_id=args["from"],
        kind="status",
    )
    await _publish_envelope(args["channel"], env)
    return {"ok": True, "data": {"message_id": env["meta"]["id"]}}


async def _start_session(args: dict) -> dict:
    worker = args["worker"]
    from_id = args["from"]
    prompt = args["prompt"]
    session_id = uuid.uuid4().hex[:8]
    session_channel = f"session.{session_id}"

    payload = {
        "action": "session_start",
        "session_id": session_id,
        "session_channel": session_channel,
        "prompt": prompt,
    }
    if args.get("model"):
        payload["model"] = args["model"]
    if args.get("provider"):
        payload["provider"] = args["provider"]
    if args.get("cwd"):
        payload["cwd"] = args["cwd"]

    env = _envelope(
        channel=session_channel,
        payload=payload,
        from_id=from_id,
        kind="message",
        to=worker,
    )
    await _publish_envelope(session_channel, env)

    # Persist the session via query API
    await _api_request("session.create", {
        "session_id": session_id,
        "orchestrator": from_id,
        "worker": worker,
        "status": "active",
        "cwd": args.get("cwd"),
        "model": args.get("model"),
        "provider": args.get("provider"),
        "created_at": _ts(),
        "updated_at": _ts(),
        "metadata": {"prompt": prompt, "timeout": args.get("timeout", 30)},
    })

    return {"ok": True, "data": {"session_id": session_id, "channel": session_channel}}


async def _send_to_session(args: dict) -> dict:
    session_id = args["session_id"]
    channel = f"session.{session_id}"
    env = _envelope(
        channel=channel,
        payload={"action": "session_send", "message": args["message"]},
        from_id=args["from"],
        kind="message",
    )
    await _publish_envelope(channel, env)
    return {"ok": True, "data": {"session_id": session_id}}


async def _close_session(args: dict) -> dict:
    session_id = args["session_id"]
    channel = f"session.{session_id}"
    env = _envelope(
        channel=channel,
        payload={"action": "session_close"},
        from_id=args["from"],
        kind="message",
    )
    await _publish_envelope(channel, env)
    await _api_request("session.update_status", {"session_id": session_id, "status": "closed"})
    return {"ok": True, "data": {"session_id": session_id, "status": "closed"}}


async def _list_sessions(args: dict) -> dict:
    params = {}
    if args.get("status"):
        params["status"] = args["status"]
    if args.get("worker"):
        params["worker"] = args["worker"]
    if args.get("orchestrator"):
        params["orchestrator"] = args["orchestrator"]
    if args.get("limit"):
        params["limit"] = args["limit"]
    return await _api_request("session.list", params)


async def _get_session(args: dict) -> dict:
    return await _api_request("session.get", {"session_id": args["session_id"]})


async def _delegate_task(args: dict) -> dict:
    to = args["to"]
    from_id = args["from"]
    prompt = args["prompt"]
    timeout = args.get("timeout", 120)
    task_id = uuid.uuid4().hex[:8]
    task_channel = f"task.{task_id}"

    # Subscribe to the task channel for the reply
    nc = await get_nc()
    reply_future: asyncio.Future = asyncio.get_event_loop().create_future()

    async def _wait_for_reply():
        sub = await nc.subscribe(f"channel.{task_channel}")
        try:
            async for msg in sub.messages:
                env = json.loads(msg.data)
                if not reply_future.done():
                    reply_future.set_result(env)
                    break
        except Exception as e:
            if not reply_future.done():
                reply_future.set_exception(e)
        finally:
            await sub.unsubscribe()

    task = asyncio.create_task(_wait_for_reply())

    # Send the task to the worker's inbox
    env = _envelope(
        channel=task_channel,
        payload={"prompt": prompt, "task_channel": task_channel},
        from_id=from_id,
        kind="message",
        to=to,
    )
    await _publish_envelope(task_channel, env)

    # Wait for reply
    try:
        result = await asyncio.wait_for(reply_future, timeout=timeout)
        return {"ok": True, "data": {
            "task_id": task_id,
            "from": from_id,
            "to": to,
            "reply": result.get("payload"),
        }}
    except asyncio.TimeoutError:
        return {"ok": False, "error": f"delegate timed out after {timeout}s"}
    finally:
        task.cancel()


async def _get_history(args: dict) -> dict:
    params = {}
    for k in ("channel", "from", "to", "kind"):
        if args.get(k):
            params[k] = args[k]
    if args.get("since"):
        params["since"] = args["since"]
    if args.get("until"):
        params["until"] = args["until"]
    if args.get("limit"):
        params["limit"] = args["limit"]
    return await _api_request("history.query", params)


async def _get_thread(args: dict) -> dict:
    return await _api_request("thread.get", {"root_id": args["root_id"]})


async def _list_pending(args: dict) -> dict:
    return await _api_request("thread.pending", {"identity": args["identity"]})


async def _create_wave(args: dict) -> dict:
    wave_id = uuid.uuid4().hex[:8]
    result = await _api_request("wave.create", {
        "wave_id": wave_id,
        "goal": args["goal"],
        "status": "active",
        "orchestrator": args["orchestrator"],
        "created_at": _ts(),
    })
    if not result.get("ok"):
        return result

    tasks_created = []
    for t in args["tasks"]:
        task_id = uuid.uuid4().hex[:8]
        r = await _api_request("wave.create_task", {
            "wave_id": wave_id,
            "task_id": task_id,
            "worker": t["worker"],
            "goal": t["goal"],
            "status": "pending",
        })
        tasks_created.append({"task_id": task_id, "worker": t["worker"], "goal": t["goal"], "ok": r.get("ok", False)})

    return {"ok": True, "data": {"wave_id": wave_id, "tasks": tasks_created}}


async def _list_waves(args: dict) -> dict:
    params = {}
    if args.get("status"):
        params["status"] = args["status"]
    return await _api_request("wave.list", params)


async def _get_wave(args: dict) -> dict:
    return await _api_request("wave.get", {"wave_id": args["wave_id"]})


async def _list_wave_tasks(args: dict) -> dict:
    return await _api_request("wave.list_tasks", {"wave_id": args["wave_id"]})


async def _get_wave_task(args: dict) -> dict:
    return await _api_request("wave.get_task", {
        "wave_id": args["wave_id"],
        "task_id": args["task_id"],
    })


async def _get_analytics(args: dict) -> dict:
    metric = args["metric"]
    op_map = {
        "message_rate": "stats.message_rate",
        "latency": "stats.latency",
        "agent_activity": "stats.agent_activity",
        "channel_hotspots": "stats.channel_hotspots",
        "error_rate": "stats.error_rate",
    }
    op = op_map.get(metric)
    if not op:
        return {"ok": False, "error": f"unknown metric: {metric}"}

    params = {"secs": args.get("secs", 3600)}
    if metric in ("message_rate", "error_rate"):
        params["interval"] = args.get("interval", "hour")
    if metric == "latency" and args.get("channel"):
        params["channel"] = args["channel"]
    if metric == "agent_activity":
        params["identity"] = args.get("identity", "")
    if metric == "channel_hotspots":
        params["limit"] = args.get("limit", 10)

    return await _api_request(op, params)


# ── Dispatch ──────────────────────────────────────────────────────

HANDLERS = {
    "list_agents": _list_agents,
    "get_agent": _get_agent,
    "send_message": _send_message,
    "send_direct": _send_direct,
    "send_status": _send_status,
    "start_session": _start_session,
    "send_to_session": _send_to_session,
    "close_session": _close_session,
    "list_sessions": _list_sessions,
    "get_session": _get_session,
    "delegate_task": _delegate_task,
    "get_history": _get_history,
    "get_thread": _get_thread,
    "list_pending": _list_pending,
    "create_wave": _create_wave,
    "list_waves": _list_waves,
    "get_wave": _get_wave,
    "list_wave_tasks": _list_wave_tasks,
    "get_wave_task": _get_wave_task,
    "get_analytics": _get_analytics,
}


# ── MCP server ────────────────────────────────────────────────────

server = Server("nats-hub")


@server.list_tools()
async def list_tools() -> list[Tool]:
    return TOOLS


@server.call_tool()
async def call_tool(name: str, arguments: dict) -> CallToolResult:
    handler = HANDLERS.get(name)
    if handler is None:
        return CallToolResult(
            content=[TextContent(type="text", text=f"Unknown tool: {name}")],
            isError=True,
        )
    try:
        result = await handler(arguments or {})
        return CallToolResult(
            content=[TextContent(type="text", text=json.dumps(result, indent=2, default=str))],
        )
    except Exception as e:
        return CallToolResult(
            content=[TextContent(type="text", text=f"Error: {e}")],
            isError=True,
        )


async def main():
    async with stdio_server() as (read_stream, write_stream):
        await server.run(read_stream, write_stream, server.create_initialization_options())


if __name__ == "__main__":
    asyncio.run(main())
