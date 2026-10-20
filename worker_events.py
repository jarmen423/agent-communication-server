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
import os
from typing import Any, Protocol

from worker_backends.proc import (
    StderrTail,
    drain_stream,
    kill_group,
    track_group,
    untrack_group,
)
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


VERIFY_TIMEOUT_SECS = 600.0  # default; WorkerConfig.verify_timeout_secs overrides
VERIFY_OUTPUT_TAIL_BYTES = 16 * 1024  # output kept for the error message
VERIFY_ERROR_TAIL_CHARS = 4000
VERIFY_PIPE_GRACE_SECS = 2.0  # after exit, wait this long for the output to close


def verify_timeout_from_env(default: float = VERIFY_TIMEOUT_SECS) -> float:
    """``NATS_HUB_VERIFY_TIMEOUT_SECS`` (seconds, > 0), else ``default``."""
    raw = os.environ.get("NATS_HUB_VERIFY_TIMEOUT_SECS")
    try:
        value = float(raw) if raw else default
    except ValueError:
        return default
    return value if value > 0 else default


async def run_verify_cmd(
    verify_cmd: str,
    *,
    cwd: str | None = None,
    timeout: float | None = VERIFY_TIMEOUT_SECS,
) -> None:
    """Run a wave task's ``verify_cmd`` (shell) to completion.

    - Its own process group, killed (TERM, then KILL) on timeout or when the
      caller is cancelled (a §4.2 cancel during verify), so no orphan survives.
    - stdout+stderr are drained as they arrive into a bounded tail buffer, so a
      chatty command can never block on a full pipe; the tail is quoted in
      the error.
    - Runs in ``cwd`` (the worker's repo/workdir).
    """
    proc = await asyncio.create_subprocess_shell(
        verify_cmd,
        stdin=asyncio.subprocess.DEVNULL,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.STDOUT,
        cwd=cwd,
        start_new_session=True,
    )
    track_group(proc.pid)
    tail = StderrTail(VERIFY_OUTPUT_TAIL_BYTES)

    async def _collect() -> None:
        drain = asyncio.ensure_future(drain_stream(proc.stdout, tail.feed))
        try:
            # proc.wait() resolves only once the pipe closes too, so watch the
            # exit status instead: a background grandchild may hold the pipe.
            while not drain.done() and proc.returncode is None:
                await asyncio.wait({drain}, timeout=0.1)
            if not drain.done():
                try:
                    await asyncio.wait_for(asyncio.shield(drain), VERIFY_PIPE_GRACE_SECS)
                except asyncio.TimeoutError:
                    await kill_group(proc, grace=0)  # SIGKILLs the leftovers
            await drain
            await proc.wait()
        finally:
            drain.cancel()

    try:
        await asyncio.wait_for(_collect(), timeout=timeout)
    except asyncio.TimeoutError:
        await kill_group(proc)
        raise RuntimeError(
            f"verify_cmd timed out after {timeout:g}s: {verify_cmd}"
            + _quoted(tail.text(VERIFY_ERROR_TAIL_CHARS))) from None
    except BaseException:
        await asyncio.shield(kill_group(proc))
        raise
    finally:
        if proc.returncode is not None:
            untrack_group(proc.pid)
    if proc.returncode != 0:
        raise RuntimeError(f"verify_cmd failed (exit {proc.returncode}): {verify_cmd}"
                           + _quoted(tail.text(VERIFY_ERROR_TAIL_CHARS)))


def _quoted(output: str) -> str:
    return f"\n--- output (tail) ---\n{output}" if output else ""


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
    verify_cwd: str | None = None,
    verify_timeout: float | None = VERIFY_TIMEOUT_SECS,
) -> tuple[str, dict[str, Any]] | None:
    """Run one worker turn, emitting started/progress/completed|error events.

    If the backend supports streaming (e.g. Grok ACP), mid-turn message/thought
    chunks are forwarded as progress events so visualizers can update live.
    ``cancel.cancel()`` stops the turn — the backend run or a running
    ``verify_cmd`` — and the result is then ``status: cancelled`` (and an
    ``error`` event with ``cancelled: true``, so wave watchers treat the task
    as terminal).

    Exactly one terminal result is published: once ``finish`` (or the
    cancelled path) has started, a later failure is logged, never answered
    with a second result.
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

    terminal_sent = False

    async def finish(kind: str, payload: dict[str, Any], status: str, event: dict[str, Any]) -> None:
        nonlocal terminal_sent
        terminal_sent = True
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
            verify = asyncio.ensure_future(
                run_verify_cmd(verify_cmd, cwd=verify_cwd, timeout=verify_timeout))
            if cancel is not None:
                cancel.attach(verify)  # a cancel during verify kills its group
            await verify
            if cancelled():
                raise asyncio.CancelledError
            await emit("milestone", {"name": "verify_passed"})
        await finish("completed", result_payload(task_id, result=result_text), done_status,
                     {"result": result_text})
        return result_text, new_ctx
    except asyncio.CancelledError:
        if not cancelled():
            raise  # worker shutdown, not a §4.2 cancel
        if terminal_sent:
            print(f"[worker] cancel of {task_id} arrived after its result; ignored")
            return None
        terminal_sent = True
        await publish_cancelled(publish, publish_event_fn, channel, task_id,
                                by=cancel.by if cancel is not None else None,
                                wave_channel=wave_channel)
        return None
    except Exception as err:
        message = str(err) or type(err).__name__
        if terminal_sent:  # publishing the result itself failed: never send a second
            print(f"[worker] publishing the result of {task_id} failed: {message}")
            return None
        await finish("error", result_payload(task_id, error=message), "error", {"error": message})
        return None
