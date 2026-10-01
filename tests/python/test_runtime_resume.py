"""Session resume stub (worker_backends/session_resume.py): feature check and
the exact hub API calls the runtime makes once T2's backend_ctx lands."""
from __future__ import annotations

import asyncio

from worker_backends import session_resume as sr


class FakeApi:
    def __init__(self, responses: dict[str, dict]) -> None:
        self.responses = responses
        self.calls: list[tuple[str, dict]] = []

    async def __call__(self, op: str, params: dict) -> dict:
        self.calls.append((op, params))
        resp = self.responses.get(op)
        if isinstance(resp, Exception):
            raise resp
        return resp or {"ok": False, "error": "unknown op"}


def test_feature_check_is_off_by_default():
    assert not sr.resume_enabled({})
    assert sr.resume_enabled({sr.RESUME_ENV: "1"})
    assert not sr.resume_enabled({sr.RESUME_ENV: "0"})


def test_backend_ctx_parsing():
    ctx = {"claude_session_id": "abc"}
    assert sr.backend_ctx_from({"ok": True, "data": {"session": {"backend_ctx": ctx}}}) == ctx
    assert sr.backend_ctx_from({"ok": True, "data": {"backend_ctx": ctx}}) == ctx
    assert sr.backend_ctx_from({"ok": True, "data": {"session": {"metadata": {"backend_ctx": ctx}}}}) == ctx
    # Pre-T2 hub: session.get has no backend_ctx → nothing to resume.
    assert sr.backend_ctx_from({"ok": True, "data": {"session": {"session_id": "s"}}}) is None
    assert sr.backend_ctx_from({"ok": False, "error": "not found"}) is None


def test_resume_all_and_save_calls():
    api = FakeApi({
        "session.list": {"ok": True, "data": {"sessions": [
            {"session_id": "s1", "orchestrator": "boss"},
            {"session_id": "s2", "metadata": {"session_channel": "wave.w.task.t"}},
        ]}},
        "session.update_backend_ctx": {"ok": True, "data": {}},
    })
    resumed: list[tuple[str, str, str]] = []

    async def resume(sid, channel, orch):
        resumed.append((sid, channel, orch))
        return True

    async def run():
        await sr.resume_all(api, "w1", resume)
        await sr.save_backend_ctx(api, "s1", {"codex_thread_id": "t"})

    asyncio.run(run())
    assert resumed == [("s1", "session.s1", "boss"), ("s2", "wave.w.task.t", "unknown")]
    assert api.calls[0] == ("session.list", {"worker": "w1", "status": "active"})
    assert api.calls[1] == ("session.update_backend_ctx",
                            {"session_id": "s1", "backend_ctx": {"codex_thread_id": "t"}})


def test_api_errors_never_raise():
    api = FakeApi({"session.get": TimeoutError("no responders"),
                   "session.list": TimeoutError("x"),
                   "session.update_backend_ctx": TimeoutError("x")})

    async def run():
        assert await sr.fetch_backend_ctx(api, "s") is None
        assert await sr.list_active_sessions(api, "w") == []
        await sr.save_backend_ctx(api, "s", {})

    asyncio.run(run())
