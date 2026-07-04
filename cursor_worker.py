#!/usr/bin/env python3
"""
nats-hub Cursor worker — subscribes to a NATS inbox channel, dispatches
tasks to a Cursor Composer 2.5 agent via the Cursor Python SDK, and
publishes results back via NATS.

Supports two modes:
  1. One-shot (hub-delegate): receive task → execute → reply on task channel
  2. Stateful sessions (hub-session): receive session_start → subscribe to
     session channel → handle multiple send/close → stay alive

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
    from cursor_sdk.types import AgentOptions

    # Track active sessions: session_id -> subscription + agent context
    # Each session gets its own Cursor agent that maintains conversation state
    active_sessions: dict[str, dict] = {}

    def make_call_cursor_fn(prompt: str, agent_ctx: dict | None = None):
        """Create a sync function that runs the Cursor SDK in a thread.

        If agent_ctx is provided with an existing agent_id, resumes that
        session. Otherwise creates a new agent.

        Returns a callable that returns (result_text, agent_id).
        """
        def _run() -> tuple:
            opts = LocalAgentOptions(cwd=args.repo)
            existing_agent_id = agent_ctx.get("agent_id") if agent_ctx else None

            if existing_agent_id:
                resume_opts = AgentOptions(
                    model=args.model,
                    local=opts,
                    api_key=api_key,
                )
                agent = Agent.resume(existing_agent_id, options=resume_opts)
            else:
                agent = Agent.create(model=args.model, api_key=api_key, local=opts)

            with agent as a:
                run = a.send(prompt)
                return run.text(), a.agent_id

        return _run

    async def call_cursor(prompt: str, agent_ctx: dict | None = None) -> tuple[str, str]:
        """Run Cursor SDK in a thread executor. Returns (result_text, agent_id)."""
        fn = make_call_cursor_fn(prompt, agent_ctx)
        loop = asyncio.get_event_loop()
        return await loop.run_in_executor(None, fn)

    # ── One-shot task handling (hub-delegate) ──────────────────

    async def process_oneshot(msg: Msg):
        """Handle a one-shot task from hub-delegate."""
        try:
            envelope = json.loads(msg.data.decode())
        except Exception as e:
            print(f"[cursor-worker] failed to decode envelope: {e}")
            return

        meta = envelope.get("meta", {})
        payload = envelope.get("payload", {})

        # Skip session messages — handled by session logic
        action = payload.get("action")
        if action in ("session_start", "session_send", "session_close"):
            await process_session_msg(msg)
            return

        prompt = payload.get("prompt") or payload.get("text") or payload.get("command")
        if not prompt:
            return

        task_channel = payload.get("task_channel") or meta.get("reply_to")
        if not task_channel:
            return

        sender = meta.get("from", "unknown")
        task_id = meta.get("id")
        print(f"[cursor-worker] oneshot from {sender} on {task_channel}: {prompt[:80]}...")

        # Status: working
        await nc.publish(f"hub.send.{task_channel}",
                         make_envelope(args.identity, None, task_channel, "status",
                                       {"status": "working"}))

        try:
            result_text, _ = await call_cursor(prompt)
            print(f"[cursor-worker] oneshot done ({len(result_text)} chars)")
            await nc.publish(f"hub.send.{task_channel}",
                             make_envelope(args.identity, None, task_channel, "message",
                                           {"result": result_text, "task_id": task_id, "status": "done"},
                                           reply_to=task_id))
            await nc.publish(f"hub.send.{task_channel}",
                             make_envelope(args.identity, None, task_channel, "status",
                                           {"status": "done"}))
        except Exception as err:
            print(f"[cursor-worker] oneshot failed: {err}")
            await nc.publish(f"hub.send.{task_channel}",
                             make_envelope(args.identity, None, task_channel, "message",
                                           {"error": str(err), "task_id": task_id, "status": "error"},
                                           reply_to=task_id))

    # ── Stateful session handling (hub-session) ────────────────

    async def process_session_msg(msg: Msg):
        """Handle session_start, session_send, session_close messages."""
        try:
            envelope = json.loads(msg.data.decode())
        except Exception as e:
            print(f"[cursor-worker] failed to decode envelope: {e}")
            return

        meta = envelope.get("meta", {})
        payload = envelope.get("payload", {})
        action = payload.get("action")
        session_id = payload.get("session_id") or meta.get("channel", "").replace("session.", "")

        if action == "session_start":
            session_channel = payload.get("session_channel", f"session.{session_id}")
            sender = meta.get("from", "unknown")
            prompt = payload.get("prompt")
            print(f"[cursor-worker] session_start: {session_id} from {sender}")

            # Subscribe to the session channel for follow-up messages
            async def session_callback(smsg: Msg):
                await process_session_msg(smsg)

            sub = await nc.subscribe(f"channel.{session_channel}", cb=session_callback)
            active_sessions[session_id] = {
                "channel": session_channel,
                "subscription": sub,
                "agent_id": None,
                "sender": sender,
            }

            # Publish status: ready
            await nc.publish(f"hub.send.{session_channel}",
                             make_envelope(args.identity, None, session_channel, "status",
                                           {"status": "ready"}))

            # If there's an initial prompt, process it
            if prompt:
                await run_session_task(session_id, session_channel, prompt, meta.get("id"))

        elif action == "session_send":
            if session_id not in active_sessions:
                print(f"[cursor-worker] session_send for unknown session {session_id}, ignoring")
                return

            session = active_sessions[session_id]
            session_channel = session["channel"]
            message = payload.get("message") or payload.get("prompt") or payload.get("text")
            if not message:
                return

            print(f"[cursor-worker] session_send: {session_id} message: {message[:80]}...")
            await run_session_task(session_id, session_channel, message, meta.get("id"))

        elif action == "session_close":
            if session_id not in active_sessions:
                return

            session = active_sessions.pop(session_id)
            session_channel = session["channel"]
            print(f"[cursor-worker] session_close: {session_id}")

            # Unsubscribe from session channel
            await session["subscription"].unsubscribe()

            # Publish status: closed
            await nc.publish(f"hub.send.{session_channel}",
                             make_envelope(args.identity, None, session_channel, "status",
                                           {"status": "closed"}))

    async def run_session_task(session_id: str, session_channel: str, prompt: str, task_id: str | None):
        """Execute a prompt within a session and publish the result."""
        session = active_sessions.get(session_id)
        if not session:
            return

        # Status: working
        await nc.publish(f"hub.send.{session_channel}",
                         make_envelope(args.identity, None, session_channel, "status",
                                       {"status": "working"}))

        try:
            print(f"[cursor-worker] calling Cursor {args.model} for session {session_id}...")
            result_text, agent_id = await call_cursor(prompt, session)
            session["agent_id"] = agent_id
            print(f"[cursor-worker] session task done ({len(result_text)} chars), agent_id={agent_id[:12]}...")

            await nc.publish(f"hub.send.{session_channel}",
                             make_envelope(args.identity, None, session_channel, "message",
                                           {"result": result_text, "task_id": task_id, "status": "done"},
                                           reply_to=task_id))
            await nc.publish(f"hub.send.{session_channel}",
                             make_envelope(args.identity, None, session_channel, "status",
                                           {"status": "idle"}))

        except Exception as err:
            print(f"[cursor-worker] session task failed: {err}")
            await nc.publish(f"hub.send.{session_channel}",
                             make_envelope(args.identity, None, session_channel, "message",
                                           {"error": str(err), "task_id": task_id, "status": "error"},
                                           reply_to=task_id))
            await nc.publish(f"hub.send.{session_channel}",
                             make_envelope(args.identity, None, session_channel, "status",
                                           {"status": "error"}))

    # ── Unified message handler ────────────────────────────────

    async def inbox_callback(msg: Msg):
        """Route incoming messages to oneshot or session handlers."""
        try:
            envelope = json.loads(msg.data.decode())
            payload = envelope.get("payload", {})
            action = payload.get("action")
            if action in ("session_start", "session_send", "session_close"):
                await process_session_msg(msg)
            else:
                await process_oneshot(msg)
        except Exception as e:
            print(f"[cursor-worker] error handling message: {e}")

    # Subscribe to inbox
    inbox_subject = f"channel.inbox.{args.identity}"
    await nc.subscribe(inbox_subject, cb=inbox_callback)
    print(f"[cursor-worker] subscribed to {inbox_subject}")

    # Optionally subscribe to a broadcast channel
    if args.channel:
        async def broadcast_callback(msg: Msg):
            await inbox_callback(msg)
        await nc.subscribe(f"channel.{args.channel}", cb=broadcast_callback)
        print(f"[cursor-worker] also subscribed to channel.{args.channel}")

    # Heartbeat
    async def heartbeat():
        while True:
            await asyncio.sleep(30)
            await nc.publish("hub.presence",
                             make_envelope(args.identity, None, "hub.presence", "status",
                                           {"identity": args.identity}))

    asyncio.create_task(heartbeat())

    print(f"[cursor-worker] ready, waiting for tasks and sessions...")

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
