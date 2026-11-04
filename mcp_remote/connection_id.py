"""A per-connection id for what the remote endpoint reports in provenance.

The endpoint is stateless, and each request has its own MCP server session, so
``clientInfo`` from ``initialize`` is gone by the first tool call and nothing
identifies the connection. So the ``initialize`` response issues an
``Mcp-Session-Id``, which MCP clients echo on every later request. It is random
(unrelated to the access token or the user) and carries the client label itself:
any replica can read it and no server keeps it.

It is not a credential. Authentication stays with the access token, and a client
can send any value here, which is fine for ``meta.origin.reported``: the hub
treats that block as unverified and applies the caps. A value in another format,
or no value, reports nothing.
"""

from __future__ import annotations

import base64
import json
import re
import uuid
from typing import Any

import hub_origin

HEADER = "mcp-session-id"
_CLIENT_MAX_BYTES = 256  # the hub drops longer values anyway
_FORMAT = re.compile(r"([0-9a-f]{32})(?:\.([A-Za-z0-9_-]{1,512}))?")


def issue(client_info: Any = None) -> str:
    """A new id, carrying the label of ``client_info`` when it fits."""
    conn_id = uuid.uuid4().hex
    label = (hub_origin.client_label(client_info) or "").encode()
    if not label or len(label) > _CLIENT_MAX_BYTES:
        return conn_id
    return f"{conn_id}.{base64.urlsafe_b64encode(label).decode().rstrip('=')}"


def reported_from(header: str | None) -> dict[str, str]:
    """The ``reported`` block for a request carrying ``header``."""
    match = _FORMAT.fullmatch(header or "")
    if match is None:
        return {}
    out = {"session": match.group(1)}
    encoded = match.group(2)
    if encoded:
        try:
            padded = encoded + "=" * (-len(encoded) % 4)
            out["client"] = base64.urlsafe_b64decode(padded).decode()
        except ValueError:
            pass
    return out


def _initialize_params(body: bytes) -> dict | None:
    if b'"initialize"' not in body:
        return None
    try:
        message = json.loads(body)
    except ValueError:
        return None
    if not isinstance(message, dict) or message.get("method") != "initialize":
        return None
    params = message.get("params")
    return params if isinstance(params, dict) else {}


async def on_initialize(scope: dict, receive: Any, send: Any) -> tuple[Any, Any]:
    """ASGI wrappers for one request: on an ``initialize`` POST with no session
    id yet, its successful response gets a new one. The body is read here and
    replayed; the MCP transport reads it whole anyway."""
    headers = dict(scope.get("headers") or [])
    if scope.get("method") != "POST" or HEADER.encode() in headers:
        return receive, send

    messages: list[dict] = []
    body = bytearray()
    while True:
        message = await receive()
        messages.append(message)
        if message["type"] != "http.request":
            break
        body += message.get("body", b"")
        if not message.get("more_body"):
            break

    async def replay() -> dict:
        return messages.pop(0) if messages else await receive()

    params = _initialize_params(bytes(body))
    if params is None:
        return replay, send
    conn_id = issue(params.get("clientInfo"))

    async def send_with_id(event: dict) -> None:
        if event["type"] == "http.response.start" and event.get("status") == 200:
            event = {**event, "headers": [*event.get("headers", []),
                                          (HEADER.encode(), conn_id.encode())]}
        await send(event)

    return replay, send_with_id
