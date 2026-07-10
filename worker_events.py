"""Structured progress events for nats-hub workers (MessageKind::event)."""

from __future__ import annotations

import asyncio
from typing import Any, Awaitable, Callable, Protocol

ProgressHandler = Callable[[str, dict[str, Any]], Awaitable[None]]


class EventPublisher(Protocol):
    async def __call__(self, channel: str, event_type: str, data: dict) -> None: ...


class StatusPublisher(Protocol):
    async def __call__(
        self, channel: str, kind: str, payload: dict, reply_to: str | None = None
    ) -> None: ...


class TaskBackend(Protocol):
    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]: ...


async def publish_event(
    publish: StatusPublisher,
    channel: str,
    event_type: str,
    data: dict,
) -> None:
    await publish(channel, "event", {"event_type": event_type, "data": data})


async def publish_event_dual(
    publish_event_fn: EventPublisher,
    channel: str,
    wave_channel: str | None,
    task_id: str | None,
    event_type: str,
    data: dict,
) -> None:
    """Publish on the task channel and optionally mirror to the wave channel."""
    await publish_event_fn(channel, event_type, data)
    if wave_channel and wave_channel != channel:
        mirrored = dict(data)
        if task_id:
            mirrored["task_id"] = task_id
        await publish_event_fn(wave_channel, event_type, mirrored)


async def run_verify_cmd(verify_cmd: str) -> None:
    proc = await asyncio.create_subprocess_shell(
        verify_cmd,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    await proc.wait()
    if proc.returncode != 0:
        raise RuntimeError(f"verify_cmd failed (exit {proc.returncode}): {verify_cmd}")


def _attach_progress_handler(backend: Any, handler: ProgressHandler) -> Callable[[], None]:
    """Best-effort: backends may expose set_progress_handler / clear_progress_handler."""
    if hasattr(backend, "set_progress_handler"):
        backend.set_progress_handler(handler)

        def _clear() -> None:
            if hasattr(backend, "clear_progress_handler"):
                backend.clear_progress_handler()
            elif hasattr(backend, "set_progress_handler"):
                backend.set_progress_handler(None)

        return _clear
    return lambda: None


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
) -> tuple[str, dict[str, Any]] | None:
    """Run one worker turn, emitting started/progress/completed|error events.

    If the backend supports streaming (e.g. Grok ACP), mid-turn message/thought
    chunks are forwarded as progress events so visualizers can update live.
    """
    emit = lambda et, data: publish_event_dual(
        publish_event_fn, channel, wave_channel, task_id, et, data
    )

    await emit("started", {"prompt": prompt[:2000] if isinstance(prompt, str) else prompt})
    await publish(channel, "status", {"status": working_status})

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

    clear_handler = _attach_progress_handler(backend, on_stream)
    try:
        await emit("progress", {"message": "calling model...", "phase": "start"})
        result_text, new_ctx = await backend.run(prompt, ctx)
        if verify_cmd:
            await emit("progress", {"message": f"running verify: {verify_cmd}", "phase": "verify"})
            await run_verify_cmd(verify_cmd)
            await emit("milestone", {"name": "verify_passed"})
        await emit("completed", {"result": result_text})
        await publish(
            channel,
            "message",
            {"result": result_text, "task_id": task_id, "status": "done"},
            reply_to=channel,
        )
        await publish(channel, "status", {"status": done_status})
        return result_text, new_ctx
    except Exception as err:
        await emit("error", {"error": str(err)})
        await publish(
            channel,
            "message",
            {"error": str(err), "task_id": task_id, "status": "error"},
            reply_to=channel,
        )
        await publish(channel, "status", {"status": "error"})
        return None
    finally:
        clear_handler()
