"""nats-hub MCP — advisory file locks (W2-D, docs/FILE_LOCKS.md).

An agent announces "I'm working on these paths" inside a ``scope`` (a repo
or project key); others see who holds what and can wait until the paths
are free. The holder is always the bound ``NATS_HUB_IDENTITY`` — tools
take no ``holder`` argument.

``wait_for_files`` subscribes to the scope's ``channel.lock.<hash>``
events AND re-checks ``lock.list`` on a poll, so a missed event can't hang
the waiter.
"""

from __future__ import annotations

import asyncio
from typing import Any

import hub_connection as conn

# Re-check cadence for wait_for_files: even with zero events arriving, the
# lock list is re-read this often, so a dropped/missed event can't hang us.
_WAIT_POLL_SECS = 2.0


def _err(msg: str) -> dict:
    return {"ok": False, "error": msg}


def _blocking(locks: list[dict], paths: list[str]) -> list[dict]:
    """Locks in `locks` conflicting with `paths`, ignoring our own holds
    (waiting on yourself is a deadlock by definition)."""
    me = conn.identity()
    return [l for l in locks
            if l.get("holder") != me
            and any(_paths_overlap(p, lp) for p in paths for lp in l.get("paths", []))]


def _normalize_path(raw: str) -> str | None:
    """Mirror of nats_hub_client::locks::normalize_path (what the lock
    service stores): trim, drop leading ``./`` and ``/``."""
    p = raw.strip()
    while p.startswith("./"):
        p = p[2:]
    p = p.lstrip("/")
    return p or None


def _paths_overlap(a: str, b: str) -> bool:
    """Mirror of nats_hub_client::locks::paths_overlap."""
    return (a.rstrip("/") == b.rstrip("/")
            or (a.endswith("/") and b.startswith(a))
            or (b.endswith("/") and a.startswith(b)))


async def _lock_files(args: dict) -> dict:
    params: dict[str, Any] = {
        "scope": args["scope"],
        "paths": args["paths"],
        # Sent for parity with the bound subject; the server stamps the
        # holder from the caller identity either way.
        "holder": conn.identity(),
    }
    for k in ("ttl_secs", "reason", "task_id"):
        if args.get(k) is not None:
            params[k] = args[k]
    res = await conn.api_request("lock.acquire", params)
    if res.get("ok") and res["data"].get("granted") is False:
        return {"ok": False,
                "error": "lock conflict — held by another agent",
                "data": res["data"]}
    return res


async def _release_files(args: dict) -> dict:
    params: dict[str, Any] = {"lock_id": args["lock_id"]}
    if args.get("force"):
        params["force"] = True
    return await conn.api_request("lock.release", params)


async def _renew_lock(args: dict) -> dict:
    params: dict[str, Any] = {"lock_id": args["lock_id"]}
    if args.get("ttl_secs") is not None:
        params["ttl_secs"] = args["ttl_secs"]
    return await conn.api_request("lock.renew", params)


async def _list_locks(args: dict) -> dict:
    params: dict[str, Any] = {}
    if args.get("scope"):
        params["scope"] = args["scope"]
    if args.get("holder"):
        params["holder"] = args["holder"]
    return await conn.api_request("lock.list", params)


async def _wait_for_files(args: dict) -> dict:
    """Block until no *other* holder locks `paths` in `scope`.

    Driven by channel.lock.<hash> events plus a periodic lock.list re-check,
    so a missed event can never hang the waiter.
    """
    # Compare exactly as the lock service does, or "./src/a.rs" would look
    # free while "src/a.rs" is held.
    scope = args["scope"].strip()
    paths = [p for p in (_normalize_path(x) for x in args["paths"]) if p]
    timeout = float(args["timeout"])

    first = await conn.api_request("lock.list", {"scope": scope})
    if not first.get("ok"):
        return first
    data = first["data"] or {}
    channel = data.get("channel")
    pending = _blocking(data.get("locks") or [], paths)
    if not pending:
        return {"ok": True, "data": {"free": True, "scope": scope, "paths": paths}}

    nc = await conn.get_nc()
    # lock.list returns the channel name ("lock.<hash>"); its subject is
    # channel.lock.<hash> (the only lock subject agents may subscribe to).
    sub = await nc.subscribe(f"channel.{channel}") if channel else None
    try:
        deadline = asyncio.get_event_loop().time() + timeout
        while pending:
            remaining = deadline - asyncio.get_event_loop().time()
            if remaining <= 0:
                return _err(
                    f"still blocked after {timeout:g}s by "
                    + ", ".join(f"{l['holder']}:{l['paths']}" for l in pending)
                )
            try:
                if sub is not None:
                    # next_msg has its own 1 s default timeout; pass ours.
                    await sub.next_msg(timeout=min(_WAIT_POLL_SECS, remaining))
                else:
                    await asyncio.sleep(min(_WAIT_POLL_SECS, remaining))
            except (asyncio.TimeoutError, TimeoutError):
                pass
            again = await conn.api_request("lock.list", {"scope": scope})
            if not again.get("ok"):
                return again
            pending = _blocking((again["data"] or {}).get("locks") or [], paths)
        return {"ok": True, "data": {"free": True, "scope": scope, "paths": paths}}
    finally:
        if sub is not None:
            try:
                await sub.unsubscribe()
            except Exception:  # noqa: BLE001 - best effort cleanup
                pass


LOCK_HANDLERS = {
    "lock_files": _lock_files,
    "release_files": _release_files,
    "renew_lock": _renew_lock,
    "list_locks": _list_locks,
    "wait_for_files": _wait_for_files,
}


def _lock_tools():
    from mcp.types import Tool, ToolAnnotations

    return [
        Tool(
            name="lock_files",
            description=(
                "Announce that you're working on files in a scope (a repo or "
                "project key) so other agents don't collide with you. Advisory "
                "coordination, not filesystem locking. Paths are repo-relative "
                "files or directory prefixes ending in '/'. All-or-nothing: on "
                "conflict returns the blocking locks. The holder is your bound "
                "identity; locks expire at ttl_secs (default 900, max 86400), "
                "when released, or when your presence is lost."
            ),
            inputSchema={
                "type": "object",
                "required": ["scope", "paths"],
                "properties": {
                    "scope": {"type": "string",
                              "description": "Repo/project key, e.g. a git remote or repo path"},
                    "paths": {"type": "array", "items": {"type": "string"}, "minItems": 1,
                              "description": "Files or dir prefixes ('src/') you will edit"},
                    "ttl_secs": {"type": "integer", "minimum": 1, "maximum": 86400,
                                 "default": 900, "description": "Lock TTL in seconds"},
                    "reason": {"type": "string"},
                    "task_id": {"type": "string", "description": "Correlate with a delegated task"},
                },
            },
        ),
        Tool(
            name="release_files",
            description=(
                "Release a lock you hold. Only the holder may release; "
                "hub admins can pass force=true to release anyone's lock."
            ),
            inputSchema={
                "type": "object",
                "required": ["lock_id"],
                "properties": {
                    "lock_id": {"type": "string"},
                    "force": {"type": "boolean", "default": False,
                              "description": "Admin-only: release a lock held by another identity"},
                },
            },
        ),
        Tool(
            name="renew_lock",
            description="Extend a lock you hold (holder only; new expiry = now + ttl_secs).",
            inputSchema={
                "type": "object",
                "required": ["lock_id"],
                "properties": {
                    "lock_id": {"type": "string"},
                    "ttl_secs": {"type": "integer", "minimum": 1, "maximum": 86400,
                                 "description": "New TTL (default: the lock's own TTL)"},
                },
            },
        ),
        Tool(
            name="list_locks",
            description=(
                "List live file locks, optionally filtered by scope and/or "
                "holder. The response includes the scope's lock event channel."
            ),
            inputSchema={
                "type": "object",
                "properties": {
                    "scope": {"type": "string"},
                    "holder": {"type": "string", "description": "Filter by holder identity"},
                },
            },
            annotations=ToolAnnotations(readOnlyHint=True),
        ),
        Tool(
            name="wait_for_files",
            description=(
                "Block until no OTHER holder locks these paths in the scope "
                "(your own locks are ignored), or timeout. Driven by lock "
                "events plus a periodic re-check, so it cannot hang on a "
                "missed event. Errors listing the blocking locks on timeout."
            ),
            inputSchema={
                "type": "object",
                "required": ["scope", "paths", "timeout"],
                "properties": {
                    "scope": {"type": "string"},
                    "paths": {"type": "array", "items": {"type": "string"}, "minItems": 1},
                    "timeout": {"type": "number", "exclusiveMinimum": 0, "maximum": 3600,
                                "description": "Seconds to wait (max 3600)"},
                },
            },
            annotations=ToolAnnotations(readOnlyHint=True),
        ),
    ]


LOCK_TOOLS = _lock_tools()
