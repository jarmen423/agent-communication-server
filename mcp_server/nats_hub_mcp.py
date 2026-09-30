#!/usr/bin/env python3
"""nats-hub MCP server — exposes hub tools to any MCP-compatible orchestrator.

Canonical source: ``mcp_server/`` at the repo root. ``scripts/dev/sync_plugins.sh``
copies this directory (plus a vendored ``nats_connect.py``) into each plugin's
``server/`` — installed plugins must stay self-contained.

Identity: ``NATS_HUB_IDENTITY`` (required) is stamped as ``meta.from`` on every
message; tool schemas take no ``from`` argument. Auth/TLS: the vendored
``nats_connect.connect_nats`` (``NATS_URL``, ``NATS_TOKEN``, TLS/creds env).

Workflow (see skills/nats-hub/SKILL.md): ``list_agents`` → ``delegate_async`` →
``wait_for_task``/``task_status`` → ``read_inbox``/``wait_for_message`` →
sessions (``start_session``/``session_replies``) → waves for parallel work.
"""

import asyncio
import json
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))

# Sibling modules live beside this file (canonical dir or a plugin's server/).
# Insert the dir so imports also work when this file is exec_module'd (Hermes).
sys.path.insert(0, _HERE)


def _default_identity() -> str | None:
    """Plugin installs default to ``<plugin-name>-agent`` derived from the
    install dir (e.g. ``codex-plugin`` → ``codex-agent``), so ``.mcp.json``
    doesn't hardcode it. The canonical ``mcp_server/`` copy has no default —
    ``NATS_HUB_IDENTITY`` must be set. Env always wins."""
    parent = os.path.basename(os.path.dirname(_HERE))
    if parent.endswith("-plugin"):
        return parent[: -len("-plugin")] + "-agent"
    return None


if (ident := _default_identity()) is not None:
    os.environ.setdefault("NATS_HUB_IDENTITY", ident)

from mcp.server import Server
from mcp.server.stdio import stdio_server
from mcp.types import CallToolResult, TextContent, Tool

from hub_tools import TOOLS
from hub_handlers import HANDLERS

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
            isError=not result.get("ok", True),
        )
    except Exception as e:
        return CallToolResult(
            content=[TextContent(type="text", text=f"Error: {e}")],
            isError=True,
        )


async def main() -> None:
    async with stdio_server() as (read_stream, write_stream):
        await server.run(
            read_stream, write_stream, server.create_initialization_options()
        )


if __name__ == "__main__":
    asyncio.run(main())
