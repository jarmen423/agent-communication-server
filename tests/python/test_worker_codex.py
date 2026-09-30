"""Codex worker: command building (exec vs exec resume), JSONL parsing,
thread-id capture, -o fallback, failures (fake `codex`), model catalog."""
from __future__ import annotations

import asyncio
import json
import os

import pytest

import codex_worker
from fixtures.helpers import BIN, ProgressRecorder
from worker_backends import model_catalog
from worker_backends.codex_cli import SESSION_CTX_KEY, CodexBackend, CodexConfig, CodexTurn

FAKE_CODEX = str(BIN / "codex")


def _backend(tmp_path, **kw) -> CodexBackend:
    kw.setdefault("codex_bin", FAKE_CODEX)
    return CodexBackend(CodexConfig(repo=tmp_path, **kw))


def test_first_turn_command(tmp_path):
    b = _backend(tmp_path, codex_bin="codex", model="gpt-x", skip_git_repo_check=True)
    turn = CodexTurn()
    try:
        cmd = b._cmd_for_turn("-v please", {}, turn)
    finally:
        turn.cleanup()
    assert cmd[:3] == ["codex", "exec", "--json"]
    assert cmd[cmd.index("-C") + 1] == str(tmp_path.resolve())
    assert cmd[cmd.index("-s") + 1] == "workspace-write"
    assert cmd[cmd.index("-m") + 1] == "gpt-x"
    assert cmd[cmd.index("-o") + 1] == turn.last_message_file
    assert "--skip-git-repo-check" in cmd
    assert "--dangerously-bypass-approvals-and-sandbox" not in cmd
    assert cmd[-2:] == ["--", "-v please"]


def test_resume_command(tmp_path):
    b = _backend(tmp_path, codex_bin="codex", sandbox="read-only")
    cmd = b._cmd_for_turn("more", {SESSION_CTX_KEY: "th-1"}, None)
    assert cmd[:4] == ["codex", "exec", "resume", "--json"]
    assert "-C" not in cmd and "-s" not in cmd  # not accepted by `exec resume`
    assert cmd[cmd.index("-c") + 1] == 'sandbox_mode="read-only"'
    assert cmd[-3:] == ["--", "th-1", "more"]


def test_bypass_only_when_explicit(tmp_path):
    safe = _backend(tmp_path, codex_bin="codex")._cmd_for_turn("x", {}, None)
    assert "--dangerously-bypass-approvals-and-sandbox" not in safe
    b = _backend(tmp_path, codex_bin="codex", dangerously_bypass=True)
    cmd = b._cmd_for_turn("x", {}, None)
    assert "--dangerously-bypass-approvals-and-sandbox" in cmd and "-s" not in cmd
    with pytest.raises(ValueError, match="unknown sandbox"):
        _backend(tmp_path, sandbox="yolo")


def test_worker_cli_defaults():
    args = codex_worker.build_parser().parse_args([])
    assert args.sandbox == "workspace-write"
    assert args.dangerously_bypass is False
    assert codex_worker.build_backend(args).spec.timeout_sec == 900.0


def test_jsonl_parsing():
    rec = ProgressRecorder()
    turn = CodexTurn()
    events = [
        {"type": "thread.started", "thread_id": "th-42"},
        {"type": "item.completed", "item": {"type": "reasoning", "text": "plan"}},
        {"type": "item.started", "item": {"type": "command_execution", "command": "ls",
                                          "status": "in_progress"}},
        {"type": "item.completed", "item": {"type": "file_change",
                                            "changes": [{"path": "a.py"}], "status": "completed"}},
        {"type": "item.completed", "item": {"type": "agent_message", "text": "done!"}},
        {"type": "turn.completed", "usage": {}},
    ]

    async def feed():
        for e in events:
            await turn.feed(json.dumps(e), rec)
        await turn.feed("garbage", rec)

    try:
        asyncio.run(feed())
        text, ctx = turn.finish("", {})
    finally:
        turn.cleanup()
    assert rec.kinds() == ["status", "thought", "tool", "tool", "message"]
    assert rec.texts("tool") == ["exec: ls in_progress", "patch: a.py completed"]
    assert text == "done!"
    assert ctx[SESSION_CTX_KEY] == "th-42"
    assert not os.path.exists(turn.last_message_file)


def test_run_and_resume_with_fake_codex(tmp_path, monkeypatch):
    argv_log = tmp_path / "argv.jsonl"
    monkeypatch.setenv("FAKE_CODEX_ARGV_LOG", str(argv_log))
    b = _backend(tmp_path)
    rec = ProgressRecorder()
    b.set_progress_handler(rec)

    async def two_turns():
        t1, ctx = await b.run("hello", {})
        t2, ctx = await b.run("again", ctx)
        return t1, t2, ctx

    t1, t2, ctx = asyncio.run(two_turns())
    assert t1 == "pong: hello"
    assert t2 == "pong: again (resumed thread-fake-0001)"
    assert ctx[SESSION_CTX_KEY] == "thread-fake-0001"
    calls = [json.loads(line) for line in argv_log.read_text().splitlines()]
    assert calls[0][:2] == ["exec", "--json"] and calls[1][:2] == ["exec", "resume"]
    assert {"status", "thought", "tool", "message"} <= set(rec.kinds())


def test_output_last_message_fallback(tmp_path, monkeypatch):
    monkeypatch.setenv("FAKE_CODEX_MODE", "ofile_only")
    text, _ = asyncio.run(_backend(tmp_path).run("hi", {}))
    assert text == "from file: hi"


def test_turn_failed_is_error(tmp_path, monkeypatch):
    monkeypatch.setenv("FAKE_CODEX_MODE", "fail")
    with pytest.raises(RuntimeError, match="exit 1.*rate limited"):
        asyncio.run(_backend(tmp_path).run("hi", {}))


def test_model_catalog_codex_and_claude(monkeypatch):
    monkeypatch.setenv("PATH", f"{BIN}{os.pathsep}{os.environ.get('PATH', '')}")
    model_catalog.clear_cache()
    codex = asyncio.run(model_catalog.list_models("codex", use_cache=False))
    assert codex["ok"] and codex["source"] == "cli"
    assert [m["value"] for m in codex["models"]] == ["gpt-fake-1", "gpt-fake-mini"]
    claude = asyncio.run(model_catalog.list_models("claude", use_cache=False))
    assert claude["source"] == "static"
    assert {"opus", "sonnet"} <= {m["value"] for m in claude["models"]}
    model_catalog.clear_cache()
