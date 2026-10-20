"""No progress cross-talk between concurrent turns on one shared backend.

Two hub sessions on the same worker share one backend object. The progress
handler used to be stored on that object (set_progress_handler), so turn B
replaced turn A's handler mid-turn: A's streamed output was published on B's
channel, and A's cleanup cleared B's handler. It is now per turn
(worker_backends.progress)."""
from __future__ import annotations

import asyncio
import json

from fixtures.helpers import BIN, FAKE_ACP
from worker_backends.claude_code import ClaudeCodeBackend, ClaudeCodeConfig
from worker_backends.grok_acp import GrokAcpBackend
from worker_backends.progress import current_progress_handler
from worker_events import execute_with_events


def _recorder():
    sent: list[tuple[str, str, dict, str | None]] = []

    async def publish(channel, kind, payload, reply_to=None):
        sent.append((channel, kind, payload, reply_to))

    async def publish_event(channel, event_type, data, reply_to=None):
        await publish(channel, "event", {"event_type": event_type, "data": data}, reply_to=reply_to)

    return sent, publish, publish_event


async def _two_turns(backend, prompts: dict[str, str]):
    """Run one turn per channel concurrently; return (sent, results)."""
    sent, publish, publish_event = _recorder()
    turns = [
        execute_with_events(
            publish=publish, publish_event_fn=publish_event, backend=backend,
            channel=channel, prompt=prompt, ctx={}, task_id=f"T-{channel}",
            working_status="working", done_status="idle")
        for channel, prompt in prompts.items()
    ]
    return sent, await asyncio.gather(*turns)


def _streamed(sent, channel: str) -> list[str]:
    """Streamed (backend) progress messages published on ``channel``."""
    return [p["data"]["message"] for ch, kind, p, _ in sent
            if ch == channel and kind == "event" and p["event_type"] == "progress"
            and p["data"].get("stream") == "message"]


def test_acp_concurrent_turns_keep_their_own_progress(tmp_path):
    """ACP stdio streams from a reader task; each chunk must reach the
    handler of the turn that owns the prompt."""
    backend = GrokAcpBackend(cwd=str(tmp_path), grok_cmd=str(FAKE_ACP), no_auto_update=False,
                             request_timeout_sec=30)
    backend._stream_min_chars = 1  # publish every chunk

    async def run():
        try:
            return await _two_turns(backend, {"session.a": "turn a", "session.b": "turn b"})
        finally:
            await backend.close()

    sent, results = asyncio.run(run())
    assert all(r is not None for r in results)
    sessions = {}
    for channel in ("session.a", "session.b"):
        streamed = _streamed(sent, channel)
        assert streamed, f"no streamed progress on {channel}: {sent}"
        # The fake agent's final chunk is JSON naming the ACP session it served.
        sids = {json.loads(m[len("hello "):])["session"] for m in streamed
                if m.startswith("hello {")}
        assert len(sids) == 1, f"{channel} saw progress from two turns: {streamed}"
        sessions[channel] = sids.pop()
    assert sessions["session.a"] != sessions["session.b"]
    # Every envelope on a channel correlates to that channel's own task.
    for ch, _, _, reply_to in sent:
        assert reply_to == f"T-{ch}"


def test_cli_concurrent_turns_keep_their_own_progress(tmp_path):
    backend = ClaudeCodeBackend(ClaudeCodeConfig(claude_bin=str(BIN / "claude"), repo=tmp_path))
    sent, results = asyncio.run(_two_turns(backend, {"session.a": "alpha", "session.b": "beta"}))
    assert [r[0] for r in results] == ["pong: alpha", "pong: beta"]
    assert _streamed(sent, "session.a") == ["pong: alpha"]
    assert _streamed(sent, "session.b") == ["pong: beta"]


def test_handler_is_scoped_to_the_turn():
    seen: list[object] = []

    class Probe:
        async def run(self, prompt, ctx):
            await asyncio.sleep(0.01)
            seen.append((prompt, current_progress_handler()))
            return prompt, ctx

    sent, results = asyncio.run(_two_turns(Probe(), {"c1": "one", "c2": "two"}))
    handlers = dict(seen)
    assert handlers["one"] is not None and handlers["two"] is not None
    assert handlers["one"] is not handlers["two"]
    assert current_progress_handler() is None  # nothing leaks outside a turn
