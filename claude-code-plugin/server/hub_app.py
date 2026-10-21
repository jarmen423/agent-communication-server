"""nats-hub MCP — the transport-independent MCP server object.

``build_server`` registers ``list_tools``/``call_tool`` for a tool list and a
validated handler table. The stdio entrypoint (``nats_hub_mcp.py``) runs it
over stdio; ``mcp_remote/`` runs the same kind of server over streamable HTTP
and passes ``bind``, an async context manager entered around every handler
call (it authenticates the HTTP request and binds the call's identity and
connection with ``hub_connection.bound``).

Handlers return ``{"ok": True, "data": ...}`` or ``{"ok": False, "error":
...}``; that dict becomes the tool result's JSON text, with ``isError`` set
from ``ok``.
"""

from __future__ import annotations

import json
from typing import Any, AsyncContextManager, Awaitable, Callable

from mcp.server import Server
from mcp.types import CallToolResult, TextContent, Tool

Handler = Callable[[dict], Awaitable[dict]]
Bind = Callable[[Server], AsyncContextManager[Any]]


def text_result(result: dict, *, indent: int | None = 2,
                ensure_ascii: bool = True) -> CallToolResult:
    """A handler result dict as an MCP tool result."""
    return CallToolResult(
        content=[TextContent(type="text", text=json.dumps(
            result, indent=indent, default=str, ensure_ascii=ensure_ascii))],
        isError=not result.get("ok", True),
    )


def error_result(text: str) -> CallToolResult:
    return CallToolResult(content=[TextContent(type="text", text=text)], isError=True)


def build_server(
    name: str,
    tools: list[Tool],
    handlers: dict[str, Handler],
    *,
    bind: Bind | None = None,
    indent: int | None = 2,
    ensure_ascii: bool = True,
    instructions: str | None = None,
) -> Server:
    """An MCP ``Server`` exposing ``tools`` backed by ``handlers``."""
    server: Server = Server(name, instructions=instructions)

    @server.list_tools()
    async def list_tools() -> list[Tool]:
        return tools

    # validate_input=False: the handlers already validate (hub_validate, with
    # actionable messages), and Hermes calls them without the MCP SDK.
    @server.call_tool(validate_input=False)
    async def call_tool(tool: str, arguments: dict) -> CallToolResult:
        handler = handlers.get(tool)
        if handler is None:
            return error_result(
                f"Unknown tool: {tool}. Available: {', '.join(sorted(handlers))}")
        try:
            if bind is None:
                result = await handler(arguments or {})
            else:
                async with bind(server):
                    result = await handler(arguments or {})
            return text_result(result, indent=indent, ensure_ascii=ensure_ascii)
        except Exception as e:  # noqa: BLE001 - every failure is a tool error
            return error_result(f"Error: {e}")

    return server
