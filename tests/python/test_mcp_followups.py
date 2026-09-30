"""Follow-up semantics for the nats-hub MCP server.

wait_for_message default scope, start_session failure reporting, the
session-start hook output shape, and the per-plugin identity default.
"""
from __future__ import annotations

import asyncio
import json
import os
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "mcp_server"))

import hub_buffers  # noqa: E402
import hub_connection as conn  # noqa: E402
import hub_handlers  # noqa: E402
import nats_hub_mcp  # noqa: E402


class _FakeSub:
    async def unsubscribe(self) -> None:
        pass


# ── wait_for_message / start_session semantics ───────────────────


class _FakeNC:
    def __init__(self) -> None:
        self.is_closed = False
        self.subs: list[tuple[str, object]] = []

    async def subscribe(self, subject: str, cb=None):
        sub = _FakeSub()
        self.subs.append((subject, sub))
        return sub

    async def flush(self) -> None:
        pass


def _fake_nc(monkeypatch) -> _FakeNC:
    nc = _FakeNC()

    async def get_nc():
        return nc

    monkeypatch.setattr(conn, "get_nc", get_nc)
    return nc


def _fresh_hub(monkeypatch, identity: str = "t-orch") -> hub_buffers.HubState:
    monkeypatch.setenv("NATS_HUB_IDENTITY", identity)
    h = hub_buffers.HubState()
    monkeypatch.setattr(hub_buffers, "_state", h)
    return h


def test_wait_for_message_only_new_by_default(monkeypatch):
    """A stale buffered DM must not satisfy wait_for_message; it only
    returns messages arriving after the call (since_seq replays)."""

    async def run():
        _fake_nc(monkeypatch)
        h = _fresh_hub(monkeypatch)
        await h.ensure_started()
        stale = {"meta": {"from": "w", "kind": "message", "id": "m1"},
                 "payload": {"x": "old"}}
        await h.inbox.buf.put(stale)

        r = await hub_handlers._wait_for_message({"timeout": 0.05})
        assert not r["ok"]

        r = await hub_handlers._wait_for_message(
            {"timeout": 0.05, "since_seq": 0})
        assert r["ok"]

    asyncio.run(run())


def test_wait_for_message_returns_new_arrival(monkeypatch):
    async def run():
        _fake_nc(monkeypatch)
        h = _fresh_hub(monkeypatch)
        await h.ensure_started()

        async def deliver():
            await asyncio.sleep(0.05)
            await h.inbox.buf.put(
                {"meta": {"from": "w", "kind": "message", "id": "m2"},
                 "payload": {"x": "fresh"}})

        t = asyncio.create_task(deliver())
        r = await hub_handlers._wait_for_message(
            {"timeout": 2.0, "from": "w"})
        await t
        assert r["ok"]

    asyncio.run(run())


def test_start_session_reports_session_create_failure(monkeypatch):
    async def run():
        _fake_nc(monkeypatch)
        _fresh_hub(monkeypatch)

        async def publish(*a, **k):
            pass

        async def api(op, params):
            return {"ok": False, "error": "db down"}

        monkeypatch.setattr(conn, "publish", publish)
        monkeypatch.setattr(conn, "api_request", api)
        r = await hub_handlers._start_session(
            {"worker": "w", "prompt": "hi"})
        assert not r["ok"] and "session.create" in r["error"]

    asyncio.run(run())


def test_session_start_hook_output_shape():
    """Hook emits hookSpecificOutput JSON even with the bus down — and
    resolves the vendored connect_nats (auth-aware) from the plugin dir."""
    hook = REPO_ROOT / "mcp_server" / "hooks" / "session_start.py"
    r = subprocess.run(
        [sys.executable, str(hook)], input="{}",
        capture_output=True, text=True, timeout=30,
        env={**os.environ, "NATS_URL": "nats://127.0.0.1:1"})
    assert r.returncode == 0, r.stderr
    out = json.loads(r.stdout)
    ctx = out["hookSpecificOutput"]["additionalContext"]
    assert out["hookSpecificOutput"]["hookEventName"] == "SessionStart"
    assert "not reachable" in ctx  # dead port → DOWN msg, fast


def test_plugin_default_identity_from_dir(monkeypatch):
    monkeypatch.setattr(
        nats_hub_mcp, "_HERE", "/x/codex-plugin/server")
    assert nats_hub_mcp._default_identity() == "codex-agent"
    monkeypatch.setattr(
        nats_hub_mcp, "_HERE", "/x/claude-code-plugin/server")
    assert nats_hub_mcp._default_identity() == "claude-code-agent"
    monkeypatch.setattr(nats_hub_mcp, "_HERE", "/x/mcp_server")
    assert nats_hub_mcp._default_identity() is None


def test_plugin_default_identity_marketplace_install_layout(tmp_path):
    """Marketplace installs live at cache/<marketplace>/<name>/<version>/ —
    the parent of server/ is a version dir, so detection must use the
    host manifest, not the directory name."""
    cases = [
        (".claude-plugin/plugin.json", "claude-code-agent"),
        (".codex-plugin/plugin.json", "codex-agent"),
        ("plugin.yaml", "hermes-agent"),
    ]
    for marker, want in cases:
        root = tmp_path / want / "cache" / "mkt" / "nats-hub" / "0.1.0"
        (root / "server").mkdir(parents=True)
        (root / marker).parent.mkdir(parents=True, exist_ok=True)
        (root / marker).write_text("{}")
        assert nats_hub_mcp._default_identity(str(root / "server")) == want
    bare = tmp_path / "bare" / "0.1.0" / "server"
    bare.mkdir(parents=True)
    assert nats_hub_mcp._default_identity(str(bare)) is None


def test_repo_plugin_dirs_resolve_by_marker():
    """Each real plugin dir in the repo carries its host marker."""
    root = Path(nats_hub_mcp.__file__).resolve().parents[1]
    for plugin, want in [("claude-code-plugin", "claude-code-agent"),
                         ("codex-plugin", "codex-agent"),
                         ("hermes-plugin", "hermes-agent")]:
        assert nats_hub_mcp._default_identity(str(root / plugin / "server")) == want

