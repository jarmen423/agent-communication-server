"""nats-hub MCP — argument validation and actionable errors for every tool.

One layer shared by the stdio MCP server and the Hermes plugin (which calls
``HANDLERS`` directly, so SDK-side validation would not cover it):

- Arguments are validated against the tool's ``inputSchema`` (JSON Schema via
  ``jsonschema``, a dependency of ``mcp``). Unknown arguments are rejected, so
  a typo such as ``timeout_secs`` fails loudly instead of being ignored. The
  error reports every problem at once, suggests the valid name for a wrong
  one (``worker`` → ``to``) and lists the valid arguments, so a model can
  fix the call in one retry.
- A legacy ``from``/``orchestrator`` identity argument is checked against
  ``NATS_HUB_IDENTITY`` first (identity is bound to the environment).
- Exceptions and hub errors get a hint saying what to do next (start the bus,
  start hub-server, set ``NATS_HUB_IDENTITY``, …).
"""

from __future__ import annotations

import difflib
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


# Names a model tends to guess for an argument. An unknown argument from a
# group suggests the group's name that the tool does take.
_SYNONYMS = (
    ("to", "worker", "agent", "recipient", "target", "worker_id", "agent_id"),
    ("prompt", "message", "task", "text", "instructions", "query", "content"),
    ("timeout", "timeout_secs", "timeout_seconds", "timeout_s", "wait", "wait_secs"),
)


def _describe(schema: dict) -> str:
    props = schema.get("properties") or {}
    required = set(schema.get("required") or [])
    parts = []
    for name, p in props.items():
        t = p.get("type", "any")
        if "enum" in p:
            t = "|".join(map(str, p["enum"]))
        extra = ", required" if name in required else ""
        if "default" in p:
            extra += f", default {p['default']}"
        parts.append(f"{name} ({t}{extra})")
    return ", ".join(parts) or "no arguments"


def _suggest(unknown: str, props: dict, given: dict) -> str | None:
    """The valid argument a wrong name most likely meant, if any."""
    free = [n for n in props if n not in given]
    for group in _SYNONYMS:
        if unknown.lower() in group:
            hits = [n for n in group if n in free]
            if hits:
                return hits[0]
    close = difflib.get_close_matches(unknown, free or list(props), n=1, cutoff=0.6)
    return close[0] if close else None


def _problems(schema: dict, clean: dict) -> list[str]:
    """Every schema violation, missing arguments first, each with a hint."""
    validator = jsonschema.validators.validator_for(schema)(schema)
    errors = sorted(validator.iter_errors(clean),
                    key=lambda e: (e.validator != "required", list(map(str, e.absolute_path))))
    props = schema.get("properties") or {}
    out = []
    for e in errors:
        where = "/".join(str(p) for p in e.absolute_path)
        msg = e.message + (f" (at '{where}')" if where else "")
        out.append(msg)
        if e.validator == "additionalProperties" and not e.absolute_path:
            out += [f"use '{v}' instead of '{k}'" for k in clean if k not in props
                    if (v := _suggest(k, props, clean))]
    return out


def validate_args(tool: str, schema: dict, args: Any) -> tuple[dict | None, str | None]:
    """Return ``(clean_args, None)`` or ``(None, actionable_error)``."""
    if args is None:
        args = {}
    if not isinstance(args, dict):
        return None, (f"{tool}: arguments must be a JSON object, got "
                      f"{type(args).__name__}. Valid arguments: {_describe(schema)}")
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
    problems = _problems(schema, clean)
    if problems:
        return None, (f"{tool}: invalid arguments — {'; '.join(problems)}. "
                      f"Valid arguments: {_describe(schema)}")
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
        if conn.is_bound():
            return (f"{msg} — the bus rejected {conn.identity()}'s credentials "
                    "or permissions.")
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
