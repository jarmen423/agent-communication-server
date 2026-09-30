"""nats-hub MCP — connection, identity and wire helpers.

Identity is stamped from ``NATS_HUB_IDENTITY`` (required — the server refuses
to connect without it, per the AGENTS.md identity convention). Auth/TLS goes
through the vendored ``nats_connect.connect_nats`` so the server can join a
token- or TLS-protected hub with the same env vars as every other Python
client (``NATS_URL``, ``NATS_TOKEN``, ``NATS_CA_FILE``, …).

The connection is lazy: the first tool call connects. Module import never
touches the network, so tool schemas load fine without a running bus.
"""

from __future__ import annotations

import asyncio
import json
import os
import uuid
from datetime import datetime, timezone
from typing import Any

import nats
from nats.aio.client import Client as NATSClient

NATS_URL = os.environ.get("NATS_URL", "nats://127.0.0.1:4222")
API_TIMEOUT = float(os.environ.get("NATS_HUB_API_TIMEOUT", "10"))

_nc: NATSClient | None = None
_connect_lock = asyncio.Lock()


def identity() -> str:
    """The orchestrator identity stamped on every envelope.

    ``NATS_HUB_IDENTITY`` is required — fail loudly instead of silently
    impersonating ``mcp-server`` or whatever a caller passes as ``from``.
    """
    ident = os.environ.get("NATS_HUB_IDENTITY", "").strip()
    if not ident:
        raise RuntimeError(
            "NATS_HUB_IDENTITY is not set. The MCP server stamps this env var "
            "as `meta.from` on every message; refusing to run without it."
        )
    return ident


def check_from_arg(args: dict) -> str | None:
    """Validate a legacy `from` argument against the env identity.

    Tool schemas no longer accept `from`, but callers that still pass it get a
    clear error unless it matches NATS_HUB_IDENTITY. Returns an error string
    or None when OK.
    """
    claimed = args.get("from") or args.get("orchestrator")
    if claimed is not None and claimed != identity():
        return (
            f"from={claimed!r} does not match NATS_HUB_IDENTITY "
            f"{identity()!r} — identity comes from the environment, not arguments"
        )
    return None


_generation = 0


def generation() -> int:
    """Bumps each time a fresh connection replaces a closed one. Subscribers
    compare it to detect that their subscriptions died with the old conn."""
    return _generation


async def get_nc() -> NATSClient:
    """Lazily connect to NATS with auth/TLS via the vendored connect_nats."""
    global _nc, _generation
    async with _connect_lock:
        if _nc is None or _nc.is_closed:
            import nats_connect

            _nc = await nats_connect.connect_nats(
                NATS_URL,
                name=identity(),
                # A dead bus must fail the tool call in seconds — the nats
                # client otherwise retries the initial connect for ~2min.
                connect_timeout=float(
                    os.environ.get("NATS_HUB_CONNECT_TIMEOUT", "5")),
                max_reconnect_attempts=int(
                    os.environ.get("NATS_HUB_CONNECT_RETRIES", "3")),
                reconnect_time_wait=0.5,
            )
            # The fail-fast settings above are for the *initial* connect only.
            # Once up, this is a long-lived connection holding the inbox, task
            # and session subscriptions: a short NATS restart must not close it
            # (nats-py re-subscribes automatically on reconnect).
            _nc.options["max_reconnect_attempts"] = -1
            _nc.options["reconnect_time_wait"] = 2.0
            _generation += 1
        return _nc


def now() -> str:
    return datetime.now(timezone.utc).isoformat()


def envelope(
    channel: str,
    payload: dict,
    *,
    kind: str = "message",
    to: str | None = None,
    reply_to: str | None = None,
) -> dict:
    """Build a nats-hub wire envelope with the env identity stamped."""
    return {
        "meta": {
            "id": str(uuid.uuid4()),
            "from": identity(),
            "channel": channel,
            "to": to,
            "timestamp": now(),
            "kind": kind,
            "reply_to": reply_to,
        },
        "payload": payload,
    }


async def publish(channel: str, env: dict) -> None:
    """Publish an envelope to ``hub.send.<channel>``."""
    nc = await get_nc()
    await nc.publish(f"hub.send.{channel}", json.dumps(env).encode())
    await nc.flush()


async def api_request(op: str, params: dict) -> dict:
    """Request-reply against the hub query API (``hub.api.<op>``)."""
    nc = await get_nc()
    req = json.dumps({"op": op, "params": params}).encode()
    try:
        reply = await nc.request(f"hub.api.{op}", req, timeout=API_TIMEOUT)
    except Exception as e:
        return {"ok": False, "error": f"query API '{op}' failed: {e}"}
    try:
        resp = json.loads(reply.data)
    except Exception as e:
        return {"ok": False, "error": f"query API '{op}' returned bad json: {e}"}
    if not resp.get("ok"):
        return {"ok": False, "error": resp.get("error", "unknown error")}
    return {"ok": True, "data": resp.get("data")}


async def close() -> None:
    global _nc
    if _nc is not None and not _nc.is_closed:
        await _nc.drain()
    _nc = None
