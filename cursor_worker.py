#!/usr/bin/env python3
"""
nats-hub Cursor worker — subscribes to a NATS inbox channel, dispatches
tasks to a Cursor Composer 2.5 agent via the Cursor Python SDK, and
publishes results back via NATS.

This is a drop-in alternative to worker.js (Cline SDK). It uses the same
nats-hub envelope protocol, so hub-delegate can route to either worker.

Usage:
    python3 cursor_worker.py --identity cursor-worker-1 --repo /home/jfrie/nats
    python3 cursor_worker.py --identity cursor-worker-1 --repo /home/jfrie/nats --model composer-2.5

Requires:
    - NATS server running on nats://127.0.0.1:4222
    - hub-server (control plane router) running
    - CURSOR_API_KEY in .env or environment
    - pip install cursor-sdk nats-py
"""

import argparse
import asyncio
import json
import os
import uuid
from datetime import datetime, timezone

# Load .env from repo root
from pathlib import Path
_env_path = Path(__file__).parent / ".env"
if _env_path.exists():
    for line in _env_path.read_text().splitlines():
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            key, _, val = line.partition("=")
            key = key.strip()
            val = val.strip().strip('"').strip("'")
            # Take first key if comma-separated
            if "," in val:
                val = val.split(",")[0].strip().strip('"').strip("'")
            if key not in os.environ:
                os.environ[key] = val

from nats.aio.client import Client as NATSClient
from nats.aio.msg import Msg


def get_api_key() -> str:
    key = os.environ.get("CURSOR_API_KEY")
    if not key:
        raise RuntimeError("CURSOR_API_KEY not set in .env or environment")
    return key.strip('"').strip("'")


def make_envelope(from_id: str, to_id: str | None, channel: str, kind: str,
                  payload: dict, reply_to: str | None = None) -> bytes:
    """Build a nats-hub wire envelope.

    Key routing rule: set to_id=None for broadcast (router routes to
    channel.<channel>), set to_id=<recipient> for DM (router routes to
    channel.inbox.<recipient>).
    """
    meta = {
        "id": str(uuid.uuid4()),
        "from": from_id,
        "channel": channel,
        "kind": kind,
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }
    if to_id:
        meta["to"] = to_id
    if reply_to:
        meta["reply_to"] = reply_to
    return json.dumps({"meta": meta, "payload": payload}).encode()


async def main():
    parser = argparse.ArgumentParser(description="Cursor SDK worker for nats-hub")
    parser.add_argument("--identity", default="cursor-worker-1")
    parser.add_argument("--model", default="composer-2.5")
    parser.add_argument("--repo", default=os.getcwd(), help="Local repo path for the agent")
    parser.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    parser.add_argument("--channel", default=None, help="Also subscribe to a broadcast channel")
    args = parser.parse_args()

    api_key = get_api_key()
    print(f"[cursor-worker] identity={args.identity} model={args.model} repo={args.repo}")

    # Connect to NATS
    nc = NATSClient()
    await nc.connect(servers=args.nats_url)
    print(f"[cursor-worker] connected to NATS")

    # Import Cursor SDK lazily (after env is loaded)
    from cursor_sdk import Agent, LocalAgentOptions

    inbox_subject = f"channel.inbox.{args.identity}"
    print(f"[cursor-worker] subscribed to {inbox_subject}")

    # Subscribe to inbox (callback pattern — nats-py Subscription doesn't
    # support async for directly)
    async def inbox_callback(msg: Msg):
        await process_message(msg)

    await nc.subscribe(inbox_subject, cb=inbox_callback)

    # Optionally subscribe to a broadcast channel
    if args.channel:
        async def broadcast_callback(msg: Msg):
            await process_message(msg)

        await nc.subscribe(f"channel.{args.channel}", cb=broadcast_callback)
        print(f"[cursor-worker] also subscribed to channel.{args.channel}")

    # Heartbeat
    async def heartbeat():
        while True:
            await asyncio.sleep(30)
            data = make_envelope(args.identity, None, "hub.presence", "status",
                                 {"identity": args.identity})
            await nc.publish("hub.presence", data)

    asyncio.create_task(heartbeat())

    async def call_cursor(prompt: str, repo: str) -> str:
        """Run the sync Cursor SDK in a thread executor so we don't block
        the asyncio event loop while the agent is working."""
        def _run():
            with Agent.create(
                model=args.model,
                api_key=api_key,
                local=LocalAgentOptions(cwd=repo),
            ) as agent:
                run = agent.send(prompt)
                return run.text()

        loop = asyncio.get_event_loop()
        return await loop.run_in_executor(None, _run)

    async def process_message(msg: Msg):
        try:
            envelope = json.loads(msg.data.decode())
        except Exception as e:
            print(f"[cursor-worker] failed to decode envelope: {e}")
            return

        meta = envelope.get("meta", {})
        payload = envelope.get("payload", {})

        prompt = payload.get("prompt") or payload.get("text") or payload.get("command")
        if not prompt:
            print("[cursor-worker] no prompt found in payload, skipping")
            return

        # Extract task channel (for hub-delegate pattern).
        # hub-delegate sets both payload.task_channel and meta.reply_to to
        # the same task.<uuid> value.
        task_channel = payload.get("task_channel") or meta.get("reply_to")
        if not task_channel:
            print("[cursor-worker] no task_channel in payload, skipping")
            return

        sender = meta.get("from", "unknown")
        task_id = meta.get("id")
        print(f"[cursor-worker] received task from {sender} on {task_channel}: {prompt[:80]}...")

        # Publish status: working
        # IMPORTANT: meta.to=None so router broadcasts to channel.task.<uuid>.
        # hub-delegate listens on channel.task.<uuid>.
        status_data = make_envelope(args.identity, None, task_channel, "status",
                                    {"status": "working"})
        await nc.publish(f"hub.send.{task_channel}", status_data)

        try:
            print(f"[cursor-worker] calling Cursor {args.model}...")
            result_text = await call_cursor(prompt, args.repo)
            print(f"[cursor-worker] task completed ({len(result_text)} chars)")

            result_payload = {
                "result": result_text,
                "task_id": task_id,
                "status": "done",
            }

            # Publish result to task channel (broadcast, no meta.to).
            # Router routes to channel.task.<uuid> where hub-delegate listens.
            task_data = make_envelope(args.identity, None, task_channel, "message",
                                      result_payload, reply_to=task_id)
            await nc.publish(f"hub.send.{task_channel}", task_data)

            # Publish status: done
            done_data = make_envelope(args.identity, None, task_channel, "status",
                                      {"status": "done"})
            await nc.publish(f"hub.send.{task_channel}", done_data)

        except Exception as err:
            print(f"[cursor-worker] task failed: {err}")

            error_payload = {
                "error": str(err),
                "task_id": task_id,
                "status": "error",
            }

            task_data = make_envelope(args.identity, None, task_channel, "message",
                                      error_payload, reply_to=task_id)
            await nc.publish(f"hub.send.{task_channel}", task_data)

            err_data = make_envelope(args.identity, None, task_channel, "status",
                                     {"status": "error"})
            await nc.publish(f"hub.send.{task_channel}", err_data)

    # Messages are handled by callbacks registered above.

    print(f"[cursor-worker] ready, waiting for tasks...")

    # Keep alive
    try:
        while True:
            await asyncio.sleep(1)
    except (KeyboardInterrupt, asyncio.CancelledError):
        pass
    finally:
        await nc.close()
        print("[cursor-worker] stopped")


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
