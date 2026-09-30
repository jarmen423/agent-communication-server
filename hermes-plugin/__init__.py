"""nats-hub Hermes plugin — wraps the shared MCP server as Hermes tools + hooks.

Installs into ~/.hermes/plugins/nats-hub/ and exposes the same tools as the
MCP server, but as one consolidated native Hermes tool (no stdio MCP needed).

Key design notes (R1 refactor):
  - The shared server module is loaded ONCE (importlib exec_module used to run
    per tool call, which reopened a NATS connection every time).
  - All async work runs on a single dedicated event loop in a daemon thread,
    so the lazily-opened NATS connection stays alive between calls.
  - Identity comes from NATS_HUB_IDENTITY like everywhere else; we default it
    to "hermes-agent" if unset.

Also provides:
  - on_session_start hook: checks bus connectivity, warns if down
  - /nats slash command: quick agent roster summary
"""

import asyncio
import concurrent.futures
import importlib.util
import json
import os
import sys
import threading
import webbrowser
from pathlib import Path

# ── Plugin configuration ──────────────────────────────────────────

NATS_URL = os.environ.get("NATS_URL", "nats://127.0.0.1:4222")
HUB_IDENTITY = os.environ.setdefault("NATS_HUB_IDENTITY", "hermes-agent")
VISUALIZER_URL = os.environ.get("NATS_HUB_VISUALIZER_URL", "http://127.0.0.1:9191")
AUTO_OPEN_VISUALIZER = os.environ.get("NATS_HUB_AUTO_OPEN", "false").lower() == "true"

# ── Shared MCP server module (loaded once) ────────────────────────

_MCP_SERVER_PATH = str(Path(__file__).parent / "server" / "nats_hub_mcp.py")
_mcp_mod = None


def _mcp():
    """Import the shared server module once and cache it."""
    global _mcp_mod
    if _mcp_mod is None:
        spec = importlib.util.spec_from_file_location("_nats_hub_mcp", _MCP_SERVER_PATH)
        if spec is None or spec.loader is None:
            raise RuntimeError(f"cannot load {_MCP_SERVER_PATH}")
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        _mcp_mod = mod
    return _mcp_mod


def _load_tool_schemas():
    return [
        {"name": t.name, "description": t.description, "parameters": t.inputSchema}
        for t in _mcp().TOOLS
    ]


# ── Single event loop for all async work ──────────────────────────

_LOOP = asyncio.new_event_loop()
_LOOP_THREAD = threading.Thread(target=_LOOP.run_forever, daemon=True)
_LOOP_STARTED = False


DEFAULT_CALL_TIMEOUT = 60.0
CALL_TIMEOUT_MARGIN = 15.0


def _call_timeout(action_params: dict) -> float:
    """Blocking actions (delegate_task, wait_for_task, wait_for_message) carry
    their own `timeout`; give them that plus a margin instead of a flat 60s."""
    try:
        t = float(action_params.get("timeout") or 0)
    except (TypeError, ValueError):
        t = 0.0
    return max(DEFAULT_CALL_TIMEOUT, t + CALL_TIMEOUT_MARGIN)


def _run_async(coro, timeout: float = DEFAULT_CALL_TIMEOUT):
    """Run a coroutine on the plugin's dedicated loop (one NATS conn)."""
    global _LOOP_STARTED
    if not _LOOP_STARTED:
        _LOOP_THREAD.start()
        _LOOP_STARTED = True
    fut = asyncio.run_coroutine_threadsafe(coro, _LOOP)
    try:
        return fut.result(timeout=timeout)
    except concurrent.futures.TimeoutError:
        fut.cancel()
        raise TimeoutError(f"nats-hub call did not finish within {timeout:.0f}s") from None


# ── Register with Hermes ──────────────────────────────────────────

ACTIONS = [
    "list_agents", "get_agent", "check_providers",
    "send_message", "send_direct", "send_status",
    "read_inbox", "wait_for_message",
    "delegate_async", "delegate_task", "task_status", "wait_for_task",
    "start_session", "send_to_session", "close_session", "session_replies",
    "list_sessions", "get_session",
    "get_history", "get_thread", "list_pending",
    "create_wave", "spawn_wave", "list_waves", "get_wave",
    "list_wave_tasks", "get_wave_task",
    "get_analytics",
]


def register(ctx):
    """Hermes plugin entry point."""

    try:
        schemas = _load_tool_schemas()
    except Exception as e:
        schemas = []
        print(f"[nats-hub] Warning: could not load tool schemas: {e}", file=sys.stderr)
    else:
        print(f"[nats-hub] loaded {len(schemas)} tool schemas from shared server",
              file=sys.stderr)

    consolidated_schema = {
        "name": "nats_hub",
        "description": (
            "NATS-hub multi-agent coordination. Actions: " + ", ".join(ACTIONS) + "."
        ),
        "parameters": {
            "type": "object",
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ACTIONS,
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
        del kwargs
        action = params.get("action", "")
        action_params = params.get("params", {})
        try:
            handler = _mcp().HANDLERS.get(action)
            if handler is None:
                return json.dumps({"ok": False, "error": f"Unknown action: {action}"})
            result = _run_async(handler(action_params), _call_timeout(action_params))
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

        def _check():
            try:
                async def _ping():
                    # Shared module's get_nc(): auth/TLS via connect_nats and
                    # reuses the tool connection instead of opening a second.
                    import hub_connection
                    await hub_connection.get_nc()
                _run_async(_ping())
                print(f"[nats-hub] Connected to bus at {NATS_URL}")
                if AUTO_OPEN_VISUALIZER:
                    webbrowser.open(VISUALIZER_URL)
            except Exception:
                print(f"[nats-hub] Warning: bus at {NATS_URL} not reachable. "
                      f"Start nats-server + hub-server to use nats-hub tools.",
                      file=sys.stderr)

        threading.Thread(target=_check, daemon=True).start()

    ctx.register_hook("on_session_start", on_session_start)

    # ── Slash command: /nats ──────────────────────────────────────

    def handle_nats_command(args, **kwargs):
        """Quick agent roster summary."""
        del args, kwargs
        try:
            result = _run_async(_mcp().HANDLERS["list_agents"]({}))
            if result.get("ok"):
                agents = (result.get("data") or {}).get("agents", [])
                if not agents:
                    return "No agents registered on the bus."
                lines = [f"**{len(agents)} agent(s) on nats-hub bus:**\n"]
                for a in agents[:20]:
                    caps = ", ".join(a.get("capabilities", [])) or "(none)"
                    lines.append(
                        f"- **{a['identity']}** — caps: {caps} — "
                        f"last seen: {a.get('last_seen', '?')}")
                if len(agents) > 20:
                    lines.append(f"\n... and {len(agents) - 20} more")
                return "\n".join(lines)
            return f"nats-hub bus error: {result.get('error', 'unknown')}"
        except Exception as e:
            return f"nats-hub error: {e}"

    ctx.register_command("nats", handle_nats_command, "Show agents on the nats-hub bus")
