"""nats-hub MCP — argument validation and actionable errors for every tool.

One layer shared by the stdio MCP server and the Hermes plugin (which calls
``HANDLERS`` directly, so SDK-side validation would not cover it):

- Arguments are validated against the tool's ``inputSchema`` (JSON Schema via
  ``jsonschema``, a dependency of ``mcp``). Unknown arguments are rejected, so
  a typo such as ``timeout_secs`` fails loudly instead of being ignored.
- A legacy ``from``/``orchestrator`` identity argument is checked against
  ``NATS_HUB_IDENTITY`` first (identity is bound to the environment).
- Exceptions and hub errors get a hint saying what to do next (start the bus,
  start hub-server, set ``NATS_HUB_IDENTITY``, …).
"""

from __future__ import annotations

import os
from typing import Any, Awaitable, Callable

import jsonschema

import hub_connection as conn

Handler = Callable[[dict], Awaitable[dict]]

# Legacy identity args: rejected on mismatch, dropped when they match — but
# only for tools that don't declare them as a *filter* (get_history `from`).
_LEGACY_IDENTITY_ARGS = ("from", "orchestrator")


def strict_schema(schema: dict) -> dict:
    """Object schemas reject unknown properties unless they say otherwise."""
    if schema.get("type") == "object" and "properties" in schema:
        schema.setdefault("additionalProperties", False)
    return schema


def _describe(schema: dict) -> str:
    props = schema.get("properties") or {}
    required = set(schema.get("required") or [])
    parts = []
    for name, p in props.items():
        t = p.get("type", "any")
        if "enum" in p:
            t = "|".join(map(str, p["enum"]))
        parts.append(f"{name} ({t}{', required' if name in required else ''})")
    return ", ".join(parts) or "no arguments"


def validate_args(tool: str, schema: dict, args: Any) -> tuple[dict | None, str | None]:
    """Return ``(clean_args, None)`` or ``(None, actionable_error)``."""
    if args is None:
        args = {}
    if not isinstance(args, dict):
        return None, (f"{tool}: arguments must be a JSON object, got "
                      f"{type(args).__name__}. Expected: {_describe(schema)}")
    props = schema.get("properties") or {}
    clean = dict(args)
    for key in _LEGACY_IDENTITY_ARGS:
        if key in clean and key not in props:
            try:
                bad = conn.check_from_arg({key: clean[key]})
            except RuntimeError as e:  # identity unset
                return None, f"{tool}: {e}"
            if bad:
                return None, f"{tool}: {bad}"
            clean.pop(key)
    try:
        jsonschema.validate(instance=clean, schema=schema)
    except jsonschema.ValidationError as e:
        where = "/".join(str(p) for p in e.absolute_path)
        loc = f" (at '{where}')" if where else ""
        return None, (f"{tool}: invalid arguments — {e.message}{loc}. "
                      f"Expected: {_describe(schema)}")
    return clean, None


def _hint(msg: str) -> str:
    """Append a next step to common failure messages."""
    low = msg.lower()
    url = os.environ.get("NATS_URL", conn.NATS_URL)
    if "nats_hub_identity" in low:
        return msg
    if "no responders" in low and ("query api" in low or "hub.api" in low):
        return (f"{msg} — hub-server is not answering on this bus ({url}). "
                "Start it (`make up` in agent-communication-server, or "
                "`hub-server --db-path <db>`), then retry.")
    if any(s in low for s in ("could not connect", "connection refused",
                              "nodename nor servname", "no servers available",
                              "connect call failed", "timed out connecting")):
        return (f"{msg} — cannot reach NATS at {url}. Start the bus (`make up`) "
                "or set NATS_URL / NATS_TOKEN / TLS env for this MCP server.")
    if "authorization" in low or "permissions violation" in low:
        return (f"{msg} — the bus rejected this identity's credentials or "
                "permissions; check NATS_TOKEN / NATS_USER / NATS_CREDS for "
                f"{os.environ.get('NATS_HUB_IDENTITY', '<unset>')}.")
    return msg


def with_validation(name: str, schema: dict, handler: Handler) -> Handler:
    async def wrapped(args: Any) -> dict:
        clean, err = validate_args(name, schema, args)
        if err is not None:
            return {"ok": False, "error": err}
        try:
            result = await handler(clean)
        except Exception as e:
            text = str(e) or type(e).__name__
            return {"ok": False, "error": _hint(f"{name}: {text}")}
        if isinstance(result, dict) and not result.get("ok", True):
            result = {**result, "error": _hint(str(result.get("error")))}
        return result

    wrapped.__name__ = f"validated_{name}"
    wrapped.__wrapped__ = handler  # type: ignore[attr-defined]
    return wrapped


def validated_handlers(tools: list, handlers: dict[str, Handler]) -> dict[str, Handler]:
    """Wrap every handler with its tool's (strict) schema. Every tool must
    have a handler and vice versa — a mismatch is a programming error."""
    by_name = {t.name: t for t in tools}
    if set(by_name) != set(handlers):
        missing = set(by_name) ^ set(handlers)
        raise RuntimeError(f"tool/handler mismatch: {sorted(missing)}")
    return {
        name: with_validation(name, strict_schema(by_name[name].inputSchema), h)
        for name, h in handlers.items()
    }
