#!/usr/bin/env python3
"""SessionStart hook for the nats-hub plugin.

Checks whether the nats-hub bus is reachable (with auth/TLS via the vendored
``nats_connect.connect_nats``, so token/TLS-protected hubs report correctly)
and injects a one-line context note. Both Claude Code and Codex consume hook
JSON under ``hookSpecificOutput.additionalContext`` — a bare
``{"context": ...}`` object is silently dropped, and plain stdout works but
the structured form is what the docs specify for SessionStart.

Canonical source: ``mcp_server/hooks/`` — synced into each plugin's
``hooks/`` by ``scripts/dev/sync_plugins.sh``. Works from either location:
the vendored ``nats_connect.py`` is found via ``../server`` (plugin install)
or ``..`` (canonical dir).

Never blocks: on failure the hook still emits JSON telling the agent the bus
is down and MCP tools will fail until it is up.
"""
import json
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
for _p in (os.path.join(_HERE, "..", "server"), _HERE + "/.."):
    if os.path.isfile(os.path.join(_p, "nats_connect.py")):
        sys.path.insert(0, os.path.abspath(_p))
        break

UP_MSG = (
    "[nats-hub] Connected to bus at {url}. Use check_providers to see which "
    "workers are alive and usable, delegate_async + wait_for_task to hand "
    "off work (cancel_task to stop it), send_direct to DM an agent, "
    "start_session for multi-turn conversations."
)
DOWN_MSG = (
    "[nats-hub] Warning: NATS bus at {url} is not reachable. In the "
    "agent-communication-server repo, `make up` starts nats-server, "
    "hub-server and echo workers. MCP tools will fail until the bus is up."
)


def main() -> None:
    try:
        json.load(sys.stdin)  # hook input on stdin; not needed
    except Exception:
        pass

    nats_url = os.environ.get("NATS_URL", "nats://127.0.0.1:4222")

    try:
        import asyncio

        from nats_connect import connect_nats

        async def check() -> None:
            nc = await connect_nats(
                nats_url,
                name="session-start-hook",
                connect_timeout=1.5,
                max_reconnect_attempts=1,
                reconnect_time_wait=0.2,
            )
            await nc.close()

        asyncio.run(check())
        context = UP_MSG.format(url=nats_url)
    except Exception:
        context = DOWN_MSG.format(url=nats_url)

    print(json.dumps({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": context,
        }
    }))


if __name__ == "__main__":
    main()
