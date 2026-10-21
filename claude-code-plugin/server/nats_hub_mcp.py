#!/usr/bin/env python3
"""nats-hub MCP server — exposes hub tools to any MCP-compatible orchestrator.

Canonical source: ``mcp_server/`` at the repo root. ``scripts/dev/sync_plugins.sh``
copies this directory (plus a vendored ``nats_connect.py``) into each plugin's
``server/`` — installed plugins must stay self-contained.

Identity: ``NATS_HUB_IDENTITY`` (required) is stamped as ``meta.from`` on every
message; tool schemas take no ``from`` argument. Auth/TLS: the vendored
``nats_connect.connect_nats`` (``NATS_URL``, ``NATS_TOKEN``, TLS/creds env).

Workflow (see skills/nats-hub/SKILL.md): ``check_providers`` → ``delegate_async``
→ ``wait_for_task``/``task_status`` (``cancel_task`` to stop) →
``read_inbox``/``wait_for_message`` → sessions → waves for parallel work.
Arguments are validated against each tool's schema (``hub_validate``).
"""

import asyncio
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))

# Sibling modules live beside this file (canonical dir or a plugin's server/).
# Insert the dir so imports also work when this file is exec_module'd (Hermes).
sys.path.insert(0, _HERE)


# Manifest each host puts at the plugin root → default identity. Detected by
# marker file, not directory name: marketplace installs live at
# ``~/.claude/plugins/cache/<marketplace>/<name>/<version>/``, where the
# parent of ``server/`` is a version string, not ``claude-code-plugin``.
_PLUGIN_MARKERS = (
    (os.path.join(".claude-plugin", "plugin.json"), "claude-code-agent"),
    (os.path.join(".codex-plugin", "plugin.json"), "codex-agent"),
    ("plugin.yaml", "hermes-agent"),
)


def _default_identity(server_dir: str | None = None) -> str | None:
    """Plugin installs default to a per-host identity (``claude-code-agent``,
    ``codex-agent``, ``hermes-agent``) so ``.mcp.json`` doesn't hardcode it.
    The canonical ``mcp_server/`` copy has no default — ``NATS_HUB_IDENTITY``
    must be set. Env always wins."""
    root = os.path.dirname(server_dir or _HERE)
    for marker, ident in _PLUGIN_MARKERS:
        if os.path.exists(os.path.join(root, marker)):
            return ident
    parent = os.path.basename(root)  # repo checkout fallback: <host>-plugin/
    if parent.endswith("-plugin"):
        return parent[: -len("-plugin")] + "-agent"
    return None


# Where the bound identity came from: "env" (NATS_HUB_IDENTITY set by the
# host/.mcp.json/shell), "plugin-manifest" (per-host default above) or None
# (canonical copy with nothing set — every bus tool will refuse to run).
if os.environ.get("NATS_HUB_IDENTITY", "").strip():
    IDENTITY_SOURCE: str | None = "env"
elif (ident := _default_identity()) is not None:
    os.environ["NATS_HUB_IDENTITY"] = ident
    IDENTITY_SOURCE = "plugin-manifest"
else:
    IDENTITY_SOURCE = None

from mcp.server.stdio import stdio_server

import hub_connection as conn
from hub_app import build_server
from hub_tools import TOOLS
from hub_handlers import HANDLERS as _RAW_HANDLERS
from hub_validate import validated_handlers


async def _whoami(args: dict) -> dict:
    try:
        ident = conn.identity()
    except RuntimeError as e:
        return {"ok": False, "error": str(e) + " Set it in the MCP server env "
                "(or install the plugin, which defaults it per host)."}
    return {"ok": True, "data": {
        "identity": ident,
        "identity_source": IDENTITY_SOURCE,
        "nats_url": conn.NATS_URL,
        "server_dir": _HERE,
        "note": "identity is bound to this server's environment and stamped "
                "as meta.from on every message; tools take no `from` argument",
    }}


# Every handler validates its arguments against the tool's (strict) schema —
# also for the Hermes plugin, which calls HANDLERS directly.
HANDLERS = validated_handlers(TOOLS, {**_RAW_HANDLERS, "whoami": _whoami})

server = build_server("nats-hub", TOOLS, HANDLERS)


async def main() -> None:
    async with stdio_server() as (read_stream, write_stream):
        await server.run(
            read_stream, write_stream, server.create_initialization_options()
        )


if __name__ == "__main__":
    asyncio.run(main())
