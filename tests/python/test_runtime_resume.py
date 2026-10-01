"""Session resume (worker_backends/session_resume.py): feature check and the
exact hub API calls (T2: session.set_backend_ctx / session.get backend_ctx)."""
from __future__ import annotations

import asyncio
import json
import os
import sys
import time
from datetime import datetime, timezone

from fixtures.helpers import BIN
from fixtures.live import fake_env, live, start_worker, stop_worker, uniq, wait_result
from worker_backends import session_resume as sr
from worker_runtime import make_envelope


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


def test_feature_check_is_on_by_default():
    assert sr.resume_enabled({})
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
        "session.set_backend_ctx": {"ok": True, "data": {}},
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
    assert api.calls[1] == ("session.set_backend_ctx",
                            {"session_id": "s1", "backend_ctx": {"codex_thread_id": "t"}})


def test_api_errors_never_raise():
    api = FakeApi({"session.get": TimeoutError("no responders"),
                   "session.list": TimeoutError("x"),
                   "session.set_backend_ctx": TimeoutError("x")})

    async def run():
        assert await sr.fetch_backend_ctx(api, "s") is None
        assert await sr.list_active_sessions(api, "w") == []
        await sr.save_backend_ctx(api, "s", {})

    asyncio.run(run())


def test_saved_ctx_drops_process_local_keys():
    api = FakeApi({"session.set_backend_ctx": {"ok": True, "data": {}}})
    ctx = {"_session_id": "s", "grok_session_id": "g1", "grok_session_id_generation": 1,
           "acp_session_id": "g1"}
    asyncio.run(sr.save_backend_ctx(api, "s", ctx))
    assert api.calls == [("session.set_backend_ctx", {
        "session_id": "s", "backend_ctx": {"grok_session_id": "g1", "acp_session_id": "g1"}})]


# ── live: a restarted worker resumes its session from backend_ctx ────


@live
def test_restarted_worker_resumes_session(tmp_path):
    """Turn 1 persists claude's session id (session.set_backend_ctx); after a
    worker restart, session.list + session.get rebuild the session and turn 2
    resumes the same Claude conversation (--resume)."""
    url = os.environ["NATS_URL"]
    worker, orch, sid = uniq("resume-claude"), uniq("orch"), uniq("s")
    channel = f"session.{sid}"
    argv = [sys.executable, "claude_worker.py", "--identity", worker, "--repo", str(tmp_path),
            "--nats-url", url, "--claude-bin", str(BIN / "claude")]
    env = fake_env(NATS_HUB_SESSION_RESUME="1")

    async def api(nc, op, params):
        req = json.dumps({"op": op, "params": params}).encode()
        return json.loads((await nc.request(f"hub.api.{orch}.{op}", req, timeout=5)).data)

    async def send(nc, to, payload):
        envelope = make_envelope(orch, to, channel, "message", payload)
        await nc.publish(f"hub.pub.{orch}.{channel}", envelope)
        await nc.flush()
        return json.loads(envelope)["meta"]["id"]

    async def scenario():
        import nats

        nc = await nats.connect(url)
        sub = await nc.subscribe(f"channel.{channel}")
        now = datetime.now(timezone.utc).isoformat()
        created = await api(nc, "session.create", {
            "session_id": sid, "orchestrator": orch, "worker": worker, "status": "active",
            "created_at": now, "updated_at": now, "metadata": {}})
        assert created.get("ok"), created
        proc = await start_worker(nc, worker, argv, env, tmp_path / "w1.log")
        try:
            tid = await send(nc, worker, {"action": "session_start", "session_id": sid,
                                          "session_channel": channel, "prompt": "one"})
            first = await wait_result(sub, tid)
            deadline = time.monotonic() + 10
            while True:  # the worker persists backend_ctx after the turn
                ctx = sr.backend_ctx_from(await api(nc, "session.get", {"session_id": sid})) or {}
                if ctx.get("claude_session_id") or time.monotonic() > deadline:
                    break
                await asyncio.sleep(0.2)
        finally:
            stop_worker(proc)
        log2 = tmp_path / "w2.log"
        proc = await start_worker(nc, worker, argv, env, log2)
        try:
            deadline = time.monotonic() + 15
            while f"resumed session {sid}" not in log2.read_text():
                assert time.monotonic() < deadline, log2.read_text()
                await asyncio.sleep(0.1)
            tid2 = await send(nc, None, {"action": "session_send", "session_id": sid,
                                         "message": "two"})
            second = await wait_result(sub, tid2)
        finally:
            stop_worker(proc)
            await nc.close()
        return first, ctx, second

    first, ctx, second = asyncio.run(scenario())
    assert first["payload"]["result"] == "pong: one"
    assert ctx == {"claude_session_id": "fake-session-0001"}
    assert second["payload"]["result"] == "pong: two (resumed fake-session-0001)"
