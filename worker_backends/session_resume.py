"""Worker-side session resume through the hub API (stub until T2 lands).

After a worker restart its in-memory sessions are gone, so a ``session_send``
on ``channel.session.<id>`` reaches nobody. T2 persists each session's
``backend_ctx`` (Claude ``claude_session_id``, Codex ``codex_thread_id``,
Cursor ``agent_id``, ACP session ids, ...) with the session record and
returns it from ``session.get``. With resume enabled, the runtime:

1. on startup, lists this worker's active sessions (``session.list``
   ``{worker, status: "active"}``), resubscribes to each session channel, and
   hydrates ``backend_ctx`` from ``session.get``;
2. on a ``session_send`` for a session it doesn't hold, fetches
   ``backend_ctx`` the same way before running the turn;
3. after every successful turn, saves the new ``backend_ctx`` (``SAVE_OP``).

Feature check: off unless ``NATS_HUB_SESSION_RESUME=1`` (or
``WorkerConfig(session_resume=True)``), because until T2 merges
``session.get`` returns no ``backend_ctx`` and ``SAVE_OP`` doesn't exist.
Every call is best effort: API errors are logged, never raised into a turn.
"""

from __future__ import annotations

import logging
import os
from typing import Any, Awaitable, Callable

logger = logging.getLogger(__name__)

RESUME_ENV = "NATS_HUB_SESSION_RESUME"
GET_OP = "session.get"
LIST_OP = "session.list"
# T2 names the write op; this is the integration point to confirm at merge.
SAVE_OP = "session.update_backend_ctx"

# (op, params) -> hub.api response {"ok": bool, "data": ..., "error": ...}
ApiRequest = Callable[[str, dict[str, Any]], Awaitable[dict[str, Any]]]


def resume_enabled(environ: dict[str, str] | None = None) -> bool:
    env = os.environ if environ is None else environ
    return env.get(RESUME_ENV, "").strip().lower() in ("1", "true", "yes", "on")


def backend_ctx_from(resp: dict[str, Any]) -> dict[str, Any] | None:
    """``backend_ctx`` from a ``session.get`` response (either nesting)."""
    if not isinstance(resp, dict) or not resp.get("ok"):
        return None
    data = resp.get("data") or {}
    session = data.get("session") if isinstance(data.get("session"), dict) else data
    for holder in (session, session.get("metadata") or {}):
        ctx = holder.get("backend_ctx") if isinstance(holder, dict) else None
        if isinstance(ctx, dict):
            return ctx
    return None


async def fetch_backend_ctx(api: ApiRequest, session_id: str) -> dict[str, Any] | None:
    try:
        return backend_ctx_from(await api(GET_OP, {"session_id": session_id}))
    except Exception as e:  # noqa: BLE001 - resume is best effort
        logger.warning("session %s: %s failed: %s", session_id, GET_OP, e)
        return None


async def list_active_sessions(api: ApiRequest, worker: str) -> list[dict[str, Any]]:
    try:
        resp = await api(LIST_OP, {"worker": worker, "status": "active"})
    except Exception as e:  # noqa: BLE001
        logger.warning("%s failed: %s", LIST_OP, e)
        return []
    if not resp.get("ok"):
        return []
    sessions = (resp.get("data") or {}).get("sessions") or []
    return [s for s in sessions if isinstance(s, dict) and s.get("session_id")]


async def resume_all(
    api: ApiRequest, worker: str, resume: Callable[[str, str, str], Awaitable[bool]]
) -> None:
    """Startup: ``resume(session_id, channel, orchestrator)`` for each of
    this worker's active sessions."""
    for record in await list_active_sessions(api, worker):
        await resume(record["session_id"], session_channel(record),
                     record.get("orchestrator") or "unknown")


def session_channel(record: dict[str, Any]) -> str:
    meta = record.get("metadata") if isinstance(record.get("metadata"), dict) else {}
    return meta.get("session_channel") or f"session.{record['session_id']}"


async def save_backend_ctx(api: ApiRequest, session_id: str, ctx: dict[str, Any]) -> None:
    try:
        resp = await api(SAVE_OP, {"session_id": session_id, "backend_ctx": ctx})
        if not resp.get("ok"):
            logger.debug("session %s: %s: %s", session_id, SAVE_OP, resp.get("error"))
    except Exception as e:  # noqa: BLE001
        logger.debug("session %s: %s failed: %s", session_id, SAVE_OP, e)
