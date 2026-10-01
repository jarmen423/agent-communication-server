"""Structured progress events for nats-hub workers (MessageKind::event).

Implements the worker side of the reply contract (refocus.md §6): every
status/event/result envelope for a task carries ``meta.reply_to = <task id>``,
and the single terminal result has payload
``{"status": "done"|"error"|"cancelled", "task_id", "result", "error"}``
(``cancelled`` per the cancel contract, refocus-iteration-2.md §4.2).

Each turn runs ``backend.run`` in its own asyncio task, with that turn's
progress handler in a context variable (``worker_backends.progress``), so
concurrent turns on one backend never see each other's progress, and a
``TurnCancel`` handle can stop exactly that turn.
"""

from __future__ import annotations

import asyncio
from typing import Any, Protocol

from worker_backends.progress import PROGRESS_HANDLER, ProgressHandler
from worker_backends.task_registry import TurnCancel

CANCELLED_ERROR = "cancelled"


class EventPublisher(Protocol):
    async def __call__(
        self, channel: str, event_type: str, data: dict, reply_to: str | None = None
    ) -> None: ...


class StatusPublisher(Protocol):
    async def __call__(
        self, channel: str, kind: str, payload: dict, reply_to: str | None = None
    ) -> None: ...


class TaskBackend(Protocol):
    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]: ...


def result_payload(
    task_id: str | None,
    *,
    result: str | None = None,
    error: str | None = None,
    cancelled: bool = False,
) -> dict[str, Any]:
    """Terminal result payload (refocus.md §6 rule 4; ``cancelled`` per §4.2)."""
    if cancelled:
        return {"status": "cancelled", "task_id": task_id, "result": None,
                "error": error or CANCELLED_ERROR}
    return {
        "status": "error" if error is not None else "done",
        "task_id": task_id,
        "result": None if error is not None else result,
        "error": error,
    }


def start_turn(
    backend: TaskBackend,
    prompt: str,
    ctx: dict[str, Any],
    handler: ProgressHandler | None = None,
    cancel: TurnCancel | None = None,
) -> asyncio.Task[tuple[str, dict[str, Any]]]:
    """Run ``backend.run`` in its own task with a per-turn progress handler.

    The task copies the current context at creation, so setting the context
    variable just around ``create_task`` scopes the handler to this turn.
    """
    token = PROGRESS_HANDLER.set(handler)
    try:
        task = asyncio.ensure_future(backend.run(prompt, ctx))
    finally:
        PROGRESS_HANDLER.reset(token)
    if cancel is not None:
        cancel.attach(task)
    return task


async def run_to_result(
    backend: TaskBackend,
    prompt: str,
    ctx: dict[str, Any],
    task_id: str | None,
    cancel: TurnCancel | None = None,
) -> dict[str, Any]:
    """Run one turn without progress events and return the terminal result
    payload. Used for plain-DM replies (rule 6), which have no task channel."""
    try:
        text, _ = await start_turn(backend, prompt, ctx, None, cancel)
        if cancel is not None and cancel.requested:
            return result_payload(task_id, cancelled=True)
        return result_payload(task_id, result=text)
    except asyncio.CancelledError:
        if cancel is not None and cancel.requested:
            return result_payload(task_id, cancelled=True)
        raise
    except Exception as err:  # noqa: BLE001 - reported to the sender
        return result_payload(task_id, error=str(err) or type(err).__name__)


async def publish_event(
    publish: StatusPublisher,
    channel: str,
    event_type: str,
    data: dict,
    reply_to: str | None = None,
) -> None:
    await publish(
        channel, "event", {"event_type": event_type, "data": data}, reply_to=reply_to
    )


async def publish_event_dual(
    publish_event_fn: EventPublisher,
    channel: str,
    wave_channel: str | None,
    task_id: str | None,
    event_type: str,
    data: dict,
) -> None:
    """Publish on the task channel and optionally mirror to the wave channel.

    Both copies carry ``meta.reply_to = task_id`` (rule 3)."""
    await publish_event_fn(channel, event_type, data, reply_to=task_id)
    if wave_channel and wave_channel != channel:
        mirrored = dict(data)
        if task_id:
            mirrored["task_id"] = task_id
        await publish_event_fn(wave_channel, event_type, mirrored, reply_to=task_id)


async def publish_cancelled(
    publish: StatusPublisher,
    publish_event_fn: EventPublisher,
    channel: str,
    task_id: str | None,
    *,
    by: str | None = None,
    wave_channel: str | None = None,
) -> None:
    """Terminal ``cancelled`` result (§4.2). The ``error`` event (with
    ``cancelled: true``) makes wave watchers treat the task as finished."""
    await publish_event_dual(publish_event_fn, channel, wave_channel, task_id, "error",
                             {"error": CANCELLED_ERROR, "cancelled": True, "by": by})
    await publish(channel, "message", result_payload(task_id, cancelled=True), reply_to=task_id)
    await publish(channel, "status", {"status": "cancelled"}, reply_to=task_id)


async def run_verify_cmd(verify_cmd: str) -> None:
    proc = await asyncio.create_subprocess_shell(
        verify_cmd,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    await proc.wait()
    if proc.returncode != 0:
        raise RuntimeError(f"verify_cmd failed (exit {proc.returncode}): {verify_cmd}")


async def execute_with_events(
    *,
    publish: StatusPublisher,
    publish_event_fn: EventPublisher,
    backend: TaskBackend,
    channel: str,
    prompt: str,
    ctx: dict[str, Any],
    task_id: str | None,
    working_status: str,
    done_status: str,
    wave_channel: str | None = None,
    verify_cmd: str | None = None,
    cancel: TurnCancel | None = None,
) -> tuple[str, dict[str, Any]] | None:
    """Run one worker turn, emitting started/progress/completed|error events.

    If the backend supports streaming (e.g. Grok ACP), mid-turn message/thought
    chunks are forwarded as progress events so visualizers can update live.
    ``cancel.cancel()`` stops the turn: the result is then ``status:
    cancelled`` (and an ``error`` event with ``cancelled: true``, so wave
    watchers treat the task as terminal).
    """
    emit = lambda et, data: publish_event_dual(
        publish_event_fn, channel, wave_channel, task_id, et, data
    )

    async def on_stream(kind: str, data: dict[str, Any]) -> None:
        # kind: message | thought | tool | status
        payload = dict(data)
        payload.setdefault("stream", kind)
        if kind == "thought":
            await emit("progress", {"message": payload.get("text") or "", **payload, "phase": "thinking"})
        elif kind == "tool":
            await emit("progress", {"message": payload.get("message") or payload.get("text") or "tool", **payload, "phase": "tool"})
        else:
            # agent message tokens / partial answers
            text = payload.get("text") or payload.get("message") or ""
            await emit("progress", {"message": text, **payload, "phase": "message"})

    async def finish(kind: str, payload: dict[str, Any], status: str, event: dict[str, Any]) -> None:
        await emit(kind, event)
        await publish(channel, "message", payload, reply_to=task_id)
        await publish(channel, "status", {"status": status}, reply_to=task_id)

    cancelled = lambda: cancel is not None and cancel.requested  # noqa: E731
    try:
        if cancelled():  # cancelled while queued: never start the backend
            raise asyncio.CancelledError
        await emit("started", {"prompt": prompt[:2000] if isinstance(prompt, str) else prompt})
        await publish(channel, "status", {"status": working_status}, reply_to=task_id)
        await emit("progress", {"message": "calling model...", "phase": "start"})
        result_text, new_ctx = await start_turn(backend, prompt, ctx, on_stream, cancel)
        if cancelled():  # backend honoured the cancel and returned normally (ACP)
            raise asyncio.CancelledError
        if verify_cmd:
            await emit("progress", {"message": f"running verify: {verify_cmd}", "phase": "verify"})
            await run_verify_cmd(verify_cmd)
            await emit("milestone", {"name": "verify_passed"})
        await finish("completed", result_payload(task_id, result=result_text), done_status,
                     {"result": result_text})
        return result_text, new_ctx
    except asyncio.CancelledError:
        if not cancelled():
            raise  # worker shutdown, not a §4.2 cancel
        await publish_cancelled(publish, publish_event_fn, channel, task_id,
                                by=cancel.by if cancel is not None else None,
                                wave_channel=wave_channel)
        return None
    except Exception as err:
        message = str(err) or type(err).__name__
        await finish("error", result_payload(task_id, error=message), "error", {"error": message})
        return None
