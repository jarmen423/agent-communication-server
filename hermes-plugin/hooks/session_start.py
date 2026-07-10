#!/usr/bin/env python3
"""SessionStart hook for nats-hub Codex plugin.

Checks whether the nats-hub bus is reachable. If not, prints a warning
but does not block — the agent can still work with cached data or
retry later via MCP tools.
"""
import json
import os
import sys

def main():
    # Read hook input from stdin
    try:
        input_data = json.load(sys.stdin)
    except Exception:
        input_data = {}

    nats_url = os.environ.get("NATS_URL", "nats://127.0.0.1:4222")

    # Try a quick connection check
    try:
        import asyncio
        import nats

        async def check():
            nc = await nats.connect(nats_url, name="codex-hook-check", connect_timeout=3)
            await nc.close()

        asyncio.run(check())
        # Bus is up — emit a context message
        output = {
            "context": f"[nats-hub] Connected to bus at {nats_url}. Use list_agents to see who's online, send_message or send_direct to communicate, delegate_task to assign work, start_session for multi-turn conversations."
        }
    except Exception:
        # Bus is down — warn but don't block
        output = {
            "context": f"[nats-hub] Warning: NATS bus at {nats_url} is not reachable. Start it with `nats-server -c config/nats-server.conf` then `cargo run --bin hub-server` in the nats-hub repo. MCP tools will fail until the bus is up."
        }

    print(json.dumps(output))


if __name__ == "__main__":
    main()
