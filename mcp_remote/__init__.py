"""Hosted remote MCP endpoint (streamable HTTP + OAuth) for web chat apps.

See docs/REMOTE_MCP.md. Tool logic is shared with the stdio server in
``mcp_server/``: its directory goes on ``sys.path`` here, the same flat
imports the plugins use (``hub_connection``, ``hub_handlers``, …).
"""

import os
import sys

MCP_SERVER_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                              "mcp_server")
if MCP_SERVER_DIR not in sys.path:
    sys.path.insert(0, MCP_SERVER_DIR)
