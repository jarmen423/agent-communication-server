"""What a worker reports in ``meta.origin.reported`` (session, model, provider).

Values come from what the worker actually knows; an unknown key is left out,
never guessed. The hub treats the block as unverified and applies the caps, so
nothing is validated here beyond trimming whitespace.

Lookup for an envelope (:func:`reported_for`): the entry for its channel, else
what a plain-DM turn collected for the task it answers, else the environment
(``NATS_HUB_REPORTED_SESSION``, ``_MODEL``, ``_MODEL_PROVIDER``).

``worker_events`` fills a channel's entry from backend ``status`` progress and
clears it when the turn's terminal result has been published. A backend reports
with :func:`report_status`, which never fails a turn.
"""

from __future__ import annotations

import contextvars
import logging
import os
import threading
from typing import Any, Awaitable, Callable, Iterable

logger = logging.getLogger(__name__)

ENV_VARS = {
    "session": "NATS_HUB_REPORTED_SESSION",
    "model": "NATS_HUB_REPORTED_MODEL",
    "model_provider": "NATS_HUB_REPORTED_MODEL_PROVIDER",
}
# Backend status-event keys, per reported key (first one present wins).
_STATUS_KEYS = {
    "session": ("session_id", "thread_id"),
    "model": ("model",),
    "model_provider": ("model_provider",),
}

_lock = threading.Lock()
_by_channel: dict[str, dict[str, str]] = {}
# (task id, reported) of this context's last plain-DM turn. The reply is
# published on a channel only the caller knows, so it is matched by reply_to.
_last_turn: contextvars.ContextVar[tuple[str, dict[str, str]] | None] = contextvars.ContextVar(
    "nats_hub_last_turn_reported", default=None)


def _clean(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    return value.strip() or None


def env_defaults() -> dict[str, str]:
    """What the environment reports. Empty or unset variables are left out."""
    found = {key: _clean(os.environ.get(var)) for key, var in ENV_VARS.items()}
    return {key: value for key, value in found.items() if value}


def from_status(data: dict[str, Any]) -> dict[str, str]:
    """The reportable values in a backend status event."""
    out: dict[str, str] = {}
    for key, names in _STATUS_KEYS.items():
        for name in names:
            value = _clean(data.get(name))
            if value:
                out[key] = value
                break
    return out


def set_reported(channel: str, reported: dict[str, str]) -> None:
    """Replace the entry for ``channel``."""
    with _lock:
        _by_channel[channel] = dict(reported)


def clear_reported(channel: str) -> None:
    with _lock:
        _by_channel.pop(channel, None)


def update_from_status(channels: Iterable[str], data: dict[str, Any]) -> None:
    """Overlay a status event's values on each channel's entry. A new entry
    starts from the environment, so runtime values override it key by key."""
    values = from_status(data)
    if not values:
        return
    defaults = env_defaults()
    with _lock:
        for channel in channels:
            _by_channel.setdefault(channel, dict(defaults)).update(values)


def remember_turn(task_id: str | None, reported: dict[str, str]) -> None:
    """Keep a plain-DM turn's values for the reply to ``task_id``."""
    _last_turn.set((task_id, dict(reported)) if task_id and reported else None)


def reported_for(channel: str, reply_to: str | None = None) -> dict[str, str]:
    """The block for an envelope on ``channel`` answering ``reply_to``."""
    with _lock:
        entry = _by_channel.get(channel)
        if entry:
            return dict(entry)
    last = _last_turn.get()
    if last is not None and reply_to and last[0] == reply_to:
        return dict(last[1])
    return env_defaults()


async def report_status(
    handler: Callable[[str, dict[str, Any]], Awaitable[None]] | None,
    *,
    session: Any = None,
    model: Any = None,
    model_provider: Any = None,
) -> None:
    """Hand known values to the turn's progress handler as a ``status`` event
    with no message (the runtime records it and publishes nothing for it)."""
    data = {key: value for key, value in (("session_id", session), ("model", model),
                                          ("model_provider", model_provider))
            if _clean(value)}
    if handler is None or not data:
        return
    try:
        await handler("status", data)
    except Exception as e:  # noqa: BLE001 - reporting must never break a turn
        logger.debug("reporting %s failed: %s", sorted(data), e)
