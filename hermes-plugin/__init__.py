"""nats-hub Hermes plugin — wraps the MCP server as Hermes tools + hooks.

Installs into ~/.hermes/plugins/nats-hub/ and exposes the same 20 tools
as the MCP server, but as native Hermes plugin tools (no stdio MCP needed).

Also provides:
  - on_session_start hook: checks bus connectivity, warns if down
  - /nats slash command: quick agent roster summary
"""

import json
import os
import subprocess
import sys
import threading
import webbrowser
from pathlib import Path

# ── Plugin configuration ──────────────────────────────────────────

NATS_URL = os.environ.get("NATS_URL", "nats://127.0.0.1:4222")
HUB_IDENTITY = os.environ.get("NATS_HUB_IDENTITY", "hermes-agent")
VISUALIZER_URL = os.environ.get("NATS_HUB_VISUALIZER_URL", "http://127.0.0.1:9191")
AUTO_OPEN_VISUALIZER = os.environ.get("NATS_HUB_AUTO_OPEN", "false").lower() == "true"

# ── MCP server path (shared with Codex/Claude Code plugins) ───────

_MCP_SERVER_PATH = str(Path(__file__).parent / "server" / "mcp_server.py")

# ── Tool schemas (mirror the MCP server) ──────────────────────────


def _load_tool_schemas():
    """Load tool schemas from the shared MCP server module."""
    import importlib.util

    spec = importlib.util.spec_from_file_location("_mcp_server", _MCP_SERVER_PATH)
    if spec is None or spec.loader is None:
        return []
    mod = importlib.util.module_from_spec(spec)
    # We need nats + mcp available — they're in the Hermes venv
    spec.loader.exec_module(mod)
    return [
        {
            "name": t.name,
            "description": t.description,
            "parameters": t.inputSchema,
        }
        for t in mod.TOOLS
    ]


# ── Tool handler — dispatches to MCP server via subprocess ────────
# (In a production version, you'd use the HubClient directly via nats-py
#  for messaging and NATS request-reply for query API, avoiding the
#  subprocess overhead. For now we reuse the MCP server for consistency.)

# Simpler approach: directly use nats-py (already installed in Hermes venv)


import asyncio as _asyncio
import uuid as _uuid
from datetime import datetime, timezone as _tz

try:
    import nats as _nats
except ImportError:
    _nats = None

_nc = None


async def _get_nc():
    global _nc
    if _nc is None or _nc.is_closed:
        _nc = await _nats.connect(NATS_URL, name=HUB_IDENTITY)
    return _nc


def _ts():
    return datetime.now(_tz.utc).isoformat()


def _envelope(channel, payload, from_id, kind="message", to=None, reply_to=None):
    return {
        "meta": {
            "id": str(_uuid.uuid4()),
            "from": from_id,
            "channel": channel,
            "to": to,
            "timestamp": _ts(),
            "kind": kind,
            "reply_to": reply_to,
        },
        "payload": payload,
    }


async def _publish_envelope(channel, env):
    nc = await _get_nc()
    await nc.publish(f"hub.send.{channel}", json.dumps(env).encode())
    await nc.flush()


async def _api_request(op, params):
    nc = await _get_nc()
    req = json.dumps({"op": op, "params": params}).encode()
    try:
        reply = await nc.request(f"hub.api.{op}", req, timeout=10)
        resp = json.loads(reply.data)
        if not resp.get("ok"):
            return {"ok": False, "error": resp.get("error", "unknown")}
        return {"ok": True, "data": resp.get("data")}
    except Exception as e:
        return {"ok": False, "error": str(e)}


def _run_async(coro):
    """Run an async coroutine in a sync context."""
    try:
        loop = _asyncio.get_event_loop()
        if loop.is_running():
            # We're inside an async context — use a thread
            import concurrent.futures
            with concurrent.futures.ThreadPoolExecutor() as pool:
                return pool.submit(_asyncio.run, coro).result(timeout=30)
    except RuntimeError:
        pass
    return _asyncio.run(coro)


# ── Register with Hermes ──────────────────────────────────────────


def register(ctx):
    """Hermes plugin entry point."""

    # Load tool schemas from the shared MCP server
    try:
        schemas = _load_tool_schemas()
    except Exception as e:
        schemas = []
        print(f"[nats-hub] Warning: could not load tool schemas: {e}", file=sys.stderr)

    # Register each tool
    async def handle_nats_tool(params, **kwargs):
        tool_name = kwargs.get("tool_name", "")
        handler_map = {
            "list_agents": lambda a: _api_request("agent.find", {
                k: v for k, v in [("capabilities", a.get("capability")),
                                   ("alive_within_secs", a.get("alive_within_secs")),
                                   ("limit", a.get("limit"))] if v is not None
            }),
            "get_agent": lambda a: _api_request("agent.get", {"identity": a["identity"]}),
            "send_message": lambda a: _publish_and_return(
                a["channel"], _envelope(a["channel"], {"message": a["message"]}, a["from"])
            ),
            "send_direct": lambda a: _publish_and_return(
                a.get("channel", "dm"),
                _envelope(a.get("channel", "dm"), {"message": a["message"]}, a["from"], to=a["to"])
            ),
            "send_status": lambda a: _publish_and_return(
                a["channel"], _envelope(a["channel"], {"status": a["status"]}, a["from"], kind="status")
            ),
            "list_sessions": lambda a: _api_request("session.list", {
                k: v for k, v in [("status", a.get("status")), ("worker", a.get("worker")),
                                   ("orchestrator", a.get("orchestrator")), ("limit", a.get("limit"))] if v
            }),
            "get_session": lambda a: _api_request("session.get", {"session_id": a["session_id"]}),
            "get_history": lambda a: _api_request("history.query", {k: v for k, v in a.items() if v}),
            "get_thread": lambda a: _api_request("thread.get", {"root_id": a["root_id"]}),
            "list_pending": lambda a: _api_request("thread.pending", {"identity": a["identity"]}),
            "list_waves": lambda a: _api_request("wave.list", {k: v for k, v in a.items() if v}),
            "get_wave": lambda a: _api_request("wave.get", {"wave_id": a["wave_id"]}),
            "list_wave_tasks": lambda a: _api_request("wave.list_tasks", {"wave_id": a["wave_id"]}),
            "get_wave_task": lambda a: _api_request("wave.get_task", {"wave_id": a["wave_id"], "task_id": a["task_id"]}),
        }
        # For now, return a JSON result — the handler will be dispatched by name
        return json.dumps({"ok": True, "data": "Tool dispatched — see MCP server for full implementation"})

    async def _publish_and_return(channel, env):
        await _publish_envelope(channel, env)
        return {"ok": True, "data": {"message_id": env["meta"]["id"]}}

    # Register a consolidated tool that routes by action
    consolidated_schema = {
        "name": "nats_hub",
        "description": (
            "NATS-hub multi-agent coordination. Actions: list_agents, get_agent, "
            "send_message, send_direct, send_status, start_session, send_to_session, "
            "close_session, list_sessions, get_session, delegate_task, get_history, "
            "get_thread, list_pending, create_wave, list_waves, get_wave, "
            "list_wave_tasks, get_wave_task, get_analytics."
        ),
        "parameters": {
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "list_agents", "get_agent", "send_message", "send_direct",
                        "send_status", "start_session", "send_to_session",
                        "close_session", "list_sessions", "get_session",
                        "delegate_task", "get_history", "get_thread",
                        "list_pending", "create_wave", "list_waves", "get_wave",
                        "list_wave_tasks", "get_wave_task", "get_analytics",
                    ],
                    "description": "Which nats-hub action to perform",
                },
                "params": {
                    "type": "object",
                    "description": "Action-specific parameters (see MCP tool schemas)",
                },
            },
        },
    }

    def handle_nats_hub(params, **kwargs):
        action = params.get("action", "")
        action_params = params.get("params", {})

        # Dispatch to the same handlers as the MCP server
        # We reuse the MCP server module's handlers directly
        try:
            import importlib.util as ilu
            spec = ilu.spec_from_file_location("_mcp_handlers", _MCP_SERVER_PATH)
            mod = ilu.module_from_spec(spec)
            spec.loader.exec_module(mod)
            handler = mod.HANDLERS.get(action)
            if handler is None:
                return json.dumps({"ok": False, "error": f"Unknown action: {action}"})
            result = _run_async(handler(action_params))
            return json.dumps(result, indent=2, default=str)
        except Exception as e:
            return json.dumps({"ok": False, "error": str(e)})

    ctx.register_tool(
        name="nats_hub",
        toolset="nats_hub",
        schema=consolidated_schema,
        handler=handle_nats_hub,
        description="NATS-hub multi-agent coordination tool",
    )

    # ── Hook: on_session_start ────────────────────────────────────

    def on_session_start(**kwargs):
        """Check bus connectivity and optionally open the visualizer."""
        del kwargs

        if _nats is None:
            print("[nats-hub] nats-py not available — tools will not work", file=sys.stderr)
            return

        # Check bus connectivity in a daemon thread (non-blocking)
        def _check():
            try:
                async def _ping():
                    nc = await _nats.connect(NATS_URL, name="hermes-hook-check", connect_timeout=3)
                    await nc.close()
                _run_async(_ping())
                print(f"[nats-hub] Connected to bus at {NATS_URL}")
                if AUTO_OPEN_VISUALIZER:
                    webbrowser.open(VISUALIZER_URL)
            except Exception:
                print(f"[nats-hub] Warning: bus at {NATS_URL} not reachable. "
                      f"Start nats-server + hub-server to use nats-hub tools.",
                      file=sys.stderr)

        t = threading.Thread(target=_check, daemon=True)
        t.start()

    ctx.register_hook("on_session_start", on_session_start)

    # ── Slash command: /nats ──────────────────────────────────────

    def handle_nats_command(args, **kwargs):
        """Quick agent roster summary."""
        del kwargs
        try:
            result = _run_async(_api_request("agent.find", {}))
            if result.get("ok"):
                agents = result["data"].get("agents", [])
                if not agents:
                    return "No agents registered on the bus."
                lines = [f"**{len(agents)} agent(s) on nats-hub bus:**\n"]
                for a in agents[:20]:
                    caps = ", ".join(a.get("capabilities", [])) or "(none)"
                    lines.append(f"- **{a['identity']}** — caps: {caps} — last seen: {a.get('last_seen', '?')}")
                if len(agents) > 20:
                    lines.append(f"\n... and {len(agents) - 20} more")
                return "\n".join(lines)
            else:
                return f"nats-hub bus error: {result.get('error', 'unknown')}"
        except Exception as e:
            return f"nats-hub error: {e}"

    ctx.register_command("nats", handle_nats_command, "Show agents on the nats-hub bus")
