"""Claude Code worker: command building, stream-json parsing, resume-id
capture, error handling (fake `claude`), plus a live hub round trip."""
from __future__ import annotations

import asyncio
import json
import os
import subprocess
import sys
import uuid

import pytest

import claude_worker
from fixtures.helpers import BIN, ProgressRecorder
from worker_backends.claude_code import (
    SESSION_CTX_KEY,
    ClaudeCodeBackend,
    ClaudeCodeConfig,
    ClaudeTurn,
    resolve_permission_mode,
)

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
FAKE_CLAUDE = str(BIN / "claude")


def _backend(tmp_path, **kw) -> ClaudeCodeBackend:
    kw.setdefault("claude_bin", FAKE_CLAUDE)
    return ClaudeCodeBackend(ClaudeCodeConfig(repo=tmp_path, **kw))


def test_command_defaults(tmp_path):
    b = _backend(tmp_path, claude_bin="claude")
    cmd = b._cmd_for_turn("-rf /", {}, None)
    assert cmd[:4] == ["claude", "-p", "--output-format", "stream-json"]
    assert "--verbose" in cmd
    assert cmd[cmd.index("--permission-mode") + 1] == "acceptEdits"
    assert cmd[-2:] == ["--", "-rf /"]
    assert "--resume" not in cmd


def test_command_model_tools_resume(tmp_path):
    b = _backend(tmp_path, claude_bin="claude", model="sonnet",
                 permission_mode="plan", allowed_tools=["Read", "Bash(git *)"])
    cmd = b._cmd_for_turn("next", {SESSION_CTX_KEY: "abc-123"}, None)
    assert cmd[cmd.index("--model") + 1] == "sonnet"
    assert cmd[cmd.index("--permission-mode") + 1] == "plan"
    # `=` form so the variadic flag cannot swallow the prompt
    assert "--allowed-tools=Read,Bash(git *)" in cmd
    assert cmd[cmd.index("--resume") + 1] == "abc-123"
    assert cmd.index("--resume") < cmd.index("--") and cmd[-1] == "next"


def test_bypass_needs_explicit_dangerous_flag(tmp_path):
    with pytest.raises(ValueError, match="dangerously-skip-permissions"):
        resolve_permission_mode("bypassPermissions", False)
    with pytest.raises(ValueError, match="unknown permission mode"):
        resolve_permission_mode("yolo", False)
    assert resolve_permission_mode("acceptEdits", True) == "bypassPermissions"
    b = _backend(tmp_path, dangerously_skip_permissions=True)
    assert b.permission_mode == "bypassPermissions"


def test_worker_cli_rejects_bypass_mode_without_flag():
    parser = claude_worker.build_parser()
    with pytest.raises(SystemExit):
        parser.parse_args(["--permission-mode", "bypassPermissions"])
    args = parser.parse_args(["--allowed-tools", "Read, Edit", "--timeout-secs", "5"])
    backend = claude_worker.build_backend(args)
    assert backend.permission_mode == "acceptEdits"
    assert backend.cfg.allowed_tools == ["Read", "Edit"]
    assert backend.spec.timeout_sec == 5.0


def test_stream_json_parsing():
    rec = ProgressRecorder()
    turn = ClaudeTurn()
    lines = [
        {"type": "system", "subtype": "init", "session_id": "sid-9", "model": "m"},
        {"type": "assistant", "message": {"content": [{"type": "thinking", "thinking": "hmm"}]}},
        {"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Edit"}]}},
        {"type": "assistant", "message": {"content": [{"type": "text", "text": "draft"}]}},
        {"type": "result", "subtype": "success", "is_error": False, "result": "final answer",
         "session_id": "sid-9"},
    ]

    async def feed():
        for obj in lines:
            await turn.feed(json.dumps(obj), rec)
        await turn.feed("not json at all", rec)

    asyncio.run(feed())
    assert rec.kinds() == ["status", "thought", "tool", "message"]
    assert rec.texts("tool") == ["tool: Edit"]
    text, ctx = turn.finish("", {"_session_id": "hub-1"})
    assert text == "final answer"
    assert ctx == {"_session_id": "hub-1", SESSION_CTX_KEY: "sid-9"}


def test_result_without_text_falls_back_to_last_message():
    turn = ClaudeTurn()
    turn.texts = ["first", "second"]
    turn.result = {"type": "result", "subtype": "success", "is_error": False, "result": ""}
    assert turn.finish("", {})[0] == "second"


def test_missing_result_event_is_error():
    with pytest.raises(RuntimeError, match="without a result event"):
        ClaudeTurn().finish("", {})


def test_run_and_resume_with_fake_claude(tmp_path, monkeypatch):
    argv_log = tmp_path / "argv.jsonl"
    monkeypatch.setenv("FAKE_CLAUDE_ARGV_LOG", str(argv_log))
    b = _backend(tmp_path)
    rec = ProgressRecorder()
    b.set_progress_handler(rec)

    async def two_turns():
        t1, ctx = await b.run("hello", {"_session_id": "hub-sess"})
        t2, ctx = await b.run("again", ctx)
        return t1, t2, ctx

    t1, t2, ctx = asyncio.run(two_turns())
    assert t1 == "pong: hello"
    assert t2 == "pong: again (resumed fake-session-0001)"
    assert ctx[SESSION_CTX_KEY] == "fake-session-0001"
    calls = [json.loads(line) for line in argv_log.read_text().splitlines()]
    assert "--resume" not in calls[0]
    assert calls[1][calls[1].index("--resume") + 1] == "fake-session-0001"
    assert {"status", "thought", "tool", "message"} <= set(rec.kinds())


@pytest.mark.parametrize("mode", ["error", "error_exit0"])
def test_error_result_raises(tmp_path, monkeypatch, mode):
    monkeypatch.setenv("FAKE_CLAUDE_MODE", mode)
    with pytest.raises(RuntimeError, match="error_during_execution.*model overloaded"):
        asyncio.run(_backend(tmp_path).run("x", {}))


def test_nonzero_exit_includes_stderr(tmp_path, monkeypatch):
    monkeypatch.setenv("FAKE_CLAUDE_MODE", "nonzero_plain")
    with pytest.raises(RuntimeError, match="exit 4.*auth expired"):
        asyncio.run(_backend(tmp_path).run("x", {}))


def test_timeout(tmp_path, monkeypatch):
    monkeypatch.setenv("FAKE_CLAUDE_MODE", "sleep")
    with pytest.raises(RuntimeError, match="timed out"):
        asyncio.run(_backend(tmp_path, timeout_sec=1.0).run("x", {}))


live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh (nats-server + hub-server)",
)


@live
def test_claude_worker_round_trip_with_fake_cli(tmp_path):
    """claude_worker.py (fake `claude` on PATH) through the real router:
    progress on the task channel, then one terminal result (refocus.md §6)."""
    import nats

    from worker_runtime import make_envelope

    async def run() -> tuple[dict, list[dict]]:
        url = os.environ["NATS_URL"]
        identity = f"claude-{uuid.uuid4().hex[:6]}"
        env = dict(os.environ, PATH=f"{BIN}{os.pathsep}{os.environ.get('PATH', '')}",
                   PYTHONUNBUFFERED="1")
        worker = subprocess.Popen(
            [sys.executable, "claude_worker.py", "--identity", identity,
             "--nats-url", url, "--repo", str(tmp_path)],
            cwd=REPO_ROOT, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        )
        nc = await nats.connect(url)
        try:
            await asyncio.sleep(2.0)  # let the worker subscribe
            task_channel = f"task.{uuid.uuid4().hex[:8]}"
            sub = await nc.subscribe(f"channel.{task_channel}")
            await nc.flush()
            task = make_envelope("pytest", identity, task_channel, "message",
                                 {"prompt": "ping", "task_channel": task_channel})
            task_id = json.loads(task)["meta"]["id"]
            await nc.publish(f"hub.send.{task_channel}", task)
            progress: list[dict] = []
            deadline = asyncio.get_running_loop().time() + 30
            while True:
                remaining = deadline - asyncio.get_running_loop().time()
                env_ = json.loads((await sub.next_msg(timeout=max(remaining, 0.1))).data)
                meta, payload = env_["meta"], env_["payload"]
                if meta["kind"] == "message" and (
                    meta.get("reply_to") == task_id or payload.get("task_id") == task_id
                ):
                    return env_, progress
                progress.append(env_)
        finally:
            await nc.close()
            worker.terminate()
            out, _ = worker.communicate(timeout=10)
            print(out.decode(errors="replace")[-3000:])

    result, progress = asyncio.run(run())
    assert result["payload"].get("status") == "done", result
    assert result["payload"].get("result") == "pong: ping"
    kinds = {e["meta"]["kind"] for e in progress}
    assert "event" in kinds and "status" in kinds
    phases = {e["payload"].get("data", {}).get("phase") for e in progress
              if e["meta"]["kind"] == "event"}
    assert "tool" in phases  # streamed from the fake stream-json
