#!/usr/bin/env python3
"""
nats-hub Hermes worker — subscribes to a NATS inbox channel, dispatches
tasks to Hermes Agent via the CLI (hermes chat -q), and publishes results
back via NATS.

This is a drop-in alternative to cursor_worker.py and worker.js. It uses
the same nats-hub envelope protocol, so hub-delegate can route to it.

Hermes supports 20+ providers (OpenRouter, Anthropic, OpenAI, Google, xAI,
ZAI/GLM, DeepSeek, etc.) and any authorized model. Pass --model and
--provider to select.

Usage:
    # Use default model from config
    python3 hermes_worker.py --identity hermes-worker-1

    # Pick a specific model + provider
    python3 hermes_worker.py --identity hermes-worker-1 --model claude-sonnet-4 --provider anthropic
    python3 hermes_worker.py --identity hermes-worker-1 --model glm-4-flash --provider zai
    python3 hermes_worker.py --identity hermes-worker-1 --model anthropic/claude-sonnet-4 --provider openrouter

    # With specific toolsets
    python3 hermes_worker.py --identity hermes-worker-1 --toolsets web,terminal

    # With skills preloaded
    python3 hermes_worker.py --identity hermes-worker-1 --skills rust-dev

Requires:
    - NATS server running on nats://127.0.0.1:4222
    - hub-server (control plane router) running
    - Hermes Agent installed (hermes chat -q works)
    - API keys configured via hermes auth / .env
"""

import argparse
import asyncio
import json
import os
import shlex
import uuid
from datetime import datetime, timezone

from nats.aio.client import Client as NATSClient
from nats.aio.msg import Msg


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
    parser = argparse.ArgumentParser(description="Hermes Agent worker for nats-hub")
    parser.add_argument("--identity", default="hermes-worker-1")
    parser.add_argument("--model", default=None, help="Model name (e.g. claude-sonnet-4, glm-4-flash). Defaults to config.")
    parser.add_argument("--provider", default=None, help="Provider (e.g. anthropic, zai, openrouter). Defaults to config.")
    parser.add_argument("--toolsets", default=None, help="Comma-separated toolsets (e.g. web,terminal)")
    parser.add_argument("--skills", default=None, help="Comma-separated skills to preload")
    parser.add_argument("--max-turns", type=int, default=15, help="Max agent turns (default 15)")
    parser.add_argument("--repo", default=os.getcwd(), help="Working directory for the agent")
    parser.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    parser.add_argument("--channel", default=None, help="Also subscribe to a broadcast channel")
    args = parser.parse_args()

    # Build the base hermes chat command. -Q suppresses banners/spinners.
    # We'll append the prompt at invocation time.
    base_cmd_parts = ["hermes", "chat", "-Q", "--max-turns", str(args.max_turns)]
    if args.model:
        base_cmd_parts.extend(["-m", args.model])
    if args.provider:
        base_cmd_parts.extend(["--provider", args.provider])
    if args.toolsets:
        base_cmd_parts.extend(["-t", args.toolsets])
    if args.skills:
        base_cmd_parts.extend(["-s", args.skills])

    print(f"[hermes-worker] identity={args.identity}")
    print(f"[hermes-worker] model={args.model or '(config default)'} provider={args.provider or '(config default)'}")
    print(f"[hermes-worker] repo={args.repo} max-turns={args.max_turns}")
    if args.toolsets:
        print(f"[hermes-worker] toolsets={args.toolsets}")
    if args.skills:
        print(f"[hermes-worker] skills={args.skills}")

    # Connect to NATS
    nc = NATSClient()
    await nc.connect(servers=args.nats_url)
    print(f"[hermes-worker] connected to NATS")

    inbox_subject = f"channel.inbox.{args.identity}"
    print(f"[hermes-worker] subscribed to {inbox_subject}")

    async def call_hermes(prompt: str) -> str:
        """Run hermes chat -q in a subprocess. Returns the response text."""
        cmd_parts = base_cmd_parts + ["-q", prompt]
        # Debug: show the command (truncated)
        cmd_display = " ".join(shlex.quote(p) for p in cmd_parts[:4]) + " -q '<prompt>'"
        print(f"[hermes-worker] exec: {cmd_display}")

        proc = await asyncio.create_subprocess_exec(
            *cmd_parts,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=args.repo,
        )
        stdout, stderr = await proc.communicate()

        if proc.returncode != 0:
            err_msg = stderr.decode().strip()
            # Even on error, hermes sometimes puts the answer in stdout
            out = stdout.decode().strip()
            if out:
                return out
            raise RuntimeError(f"hermes exit {proc.returncode}: {err_msg[:500]}")

        # Hermes -Q outputs: optional "session_id: ..." line, then the response.
        # We extract the response, skipping session_id and warning lines.
        output = stdout.decode().strip()
        lines = output.splitlines()
        response_lines = [
            line for line in lines
            if not line.startswith("session_id:")
            and not line.startswith("Warning:")
            and not line.startswith("⚠️")
        ]
        return "\n".join(response_lines).strip() or output

    async def process_message(msg: Msg):
        try:
            envelope = json.loads(msg.data.decode())
        except Exception as e:
            print(f"[hermes-worker] failed to decode envelope: {e}")
            return

        meta = envelope.get("meta", {})
        payload = envelope.get("payload", {})

        prompt = payload.get("prompt") or payload.get("text") or payload.get("command")
        if not prompt:
            print("[hermes-worker] no prompt found in payload, skipping")
            return

        # Extract task channel (for hub-delegate pattern).
        # hub-delegate sets both payload.task_channel and meta.reply_to to
        # the same task.<uuid> value.
        task_channel = payload.get("task_channel") or meta.get("reply_to")
        if not task_channel:
            print("[hermes-worker] no task_channel in payload, skipping")
            return

        sender = meta.get("from", "unknown")
        task_id = meta.get("id")
        print(f"[hermes-worker] received task from {sender} on {task_channel}: {prompt[:80]}...")

        # Publish status: working (broadcast to task channel)
        status_data = make_envelope(args.identity, None, task_channel, "status",
                                    {"status": "working"})
        await nc.publish(f"hub.send.{task_channel}", status_data)

        try:
            result_text = await call_hermes(prompt)
            print(f"[hermes-worker] task completed ({len(result_text)} chars)")

            result_payload = {
                "result": result_text,
                "task_id": task_id,
                "status": "done",
            }

            # Publish result to task channel (broadcast, no meta.to)
            task_data = make_envelope(args.identity, None, task_channel, "message",
                                      result_payload, reply_to=task_id)
            await nc.publish(f"hub.send.{task_channel}", task_data)

            # Publish status: done
            done_data = make_envelope(args.identity, None, task_channel, "status",
                                      {"status": "done"})
            await nc.publish(f"hub.send.{task_channel}", done_data)

        except Exception as err:
            print(f"[hermes-worker] task failed: {err}")

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

    # Subscribe to inbox via callback
    async def inbox_callback(msg: Msg):
        await process_message(msg)

    await nc.subscribe(inbox_subject, cb=inbox_callback)

    # Optionally subscribe to a broadcast channel
    if args.channel:
        async def broadcast_callback(msg: Msg):
            await process_message(msg)

        await nc.subscribe(f"channel.{args.channel}", cb=broadcast_callback)
        print(f"[hermes-worker] also subscribed to channel.{args.channel}")

    # Heartbeat
    async def heartbeat():
        while True:
            await asyncio.sleep(30)
            data = make_envelope(args.identity, None, "hub.presence", "status",
                                 {"identity": args.identity})
            await nc.publish("hub.presence", data)

    asyncio.create_task(heartbeat())

    print(f"[hermes-worker] ready, waiting for tasks...")

    # Keep alive
    try:
        while True:
            await asyncio.sleep(1)
    except (KeyboardInterrupt, asyncio.CancelledError):
        pass
    finally:
        await nc.close()
        print("[hermes-worker] stopped")


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
