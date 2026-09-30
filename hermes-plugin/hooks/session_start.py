#!/usr/bin/env python3
"""SessionStart hook for the nats-hub plugin.

Checks whether the nats-hub bus is reachable and injects a one-line context
note. Claude Code consumes hook JSON only under
``hookSpecificOutput.additionalContext`` — a bare ``{"context": ...}`` object
is silently dropped, and plain stdout works but the structured form is what
the docs specify for SessionStart.

Never blocks: on failure the hook still emits JSON telling the agent the bus
is down and MCP tools will fail until it is up.
"""
import json
import os
import sys

UP_MSG = (
    "[nats-hub] Connected to bus at {url}. Use list_agents to see who's "
    "online, delegate_async + wait_for_task to hand off work, send_direct "
    "to DM an agent, start_session for multi-turn conversations."
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

        import nats

        async def check() -> None:
            nc = await nats.connect(
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
