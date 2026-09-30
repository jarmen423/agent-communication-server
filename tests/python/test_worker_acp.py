"""ACP stdio hardening (grok/opencode share worker_backends/acp_stdio.py):
stderr drain, no fs/terminal capabilities, permission choice by option kind,
dead-on-EOF + restart."""
from __future__ import annotations

import asyncio
import json

import pytest

from fixtures.helpers import FAKE_ACP, wait_dead
from worker_backends.acp_stdio import (
    CLIENT_CAPABILITIES,
    choose_permission_option,
    permission_result,
)
from worker_backends.grok_acp import GrokAcpBackend
from worker_backends.opencode_acp import OpencodeAcpBackend

OPTIONS = [
    {"optionId": "r1", "kind": "reject_once"},
    {"optionId": "a1", "kind": "allow_once"},
    {"optionId": "aa", "kind": "allow_always"},
    {"optionId": "ra", "kind": "reject_always"},
]


@pytest.mark.parametrize("policy,expected", [
    ("allow_always", "aa"), ("allow_once", "a1"), ("reject", "r1"),
])
def test_choose_permission_option_by_kind(policy, expected):
    assert choose_permission_option(OPTIONS, policy) == expected


def test_permission_fallbacks():
    only_once = [{"optionId": "x", "kind": "allow_once"}]
    assert choose_permission_option(only_once, "allow_always") == "x"
    assert choose_permission_option(only_once, "reject") is None
    assert permission_result({"options": only_once}, "reject") == {"outcome": {"outcome": "cancelled"}}
    assert permission_result({}, "allow_once") == {"outcome": {"outcome": "cancelled"}}
    assert permission_result({"options": OPTIONS}, "allow_once") == {
        "outcome": {"outcome": "selected", "optionId": "a1"}}


def test_no_fs_or_terminal_capabilities_advertised():
    assert CLIENT_CAPABILITIES["terminal"] is False
    assert not any(CLIENT_CAPABILITIES["fs"].values())


def _reply(text: str) -> dict:
    assert text.startswith("hello "), text
    return json.loads(text[len("hello "):])


@pytest.mark.parametrize("make", [
    lambda cwd: GrokAcpBackend(cwd=cwd, grok_cmd=str(FAKE_ACP), no_auto_update=False,
                               permission_policy="allow_once", request_timeout_sec=20),
    lambda cwd: OpencodeAcpBackend(cwd=cwd, opencode_cmd=str(FAKE_ACP), model="m/x",
                                   permission_policy="allow_once", request_timeout_sec=20),
], ids=["grok", "opencode"])
def test_acp_backend_end_to_end(tmp_path, make):
    backend = make(str(tmp_path))

    async def scenario():
        try:
            # 1. basic turn; the fake floods stderr on initialize (must not deadlock)
            t1, ctx = await asyncio.wait_for(backend.run("hi", {}), 20)
            r1 = _reply(t1)
            assert r1["caps"] == CLIENT_CAPABILITIES
            assert "stderr noise" in backend.stderr.text()
            assert len(backend.stderr.text()) <= backend.stderr.max_bytes

            # 2. same session; permission answered by kind, fs request refused
            t2, ctx = await backend.run("permission and fs please", ctx)
            r2 = _reply(t2)
            assert r2["session"] == r1["session"] and r2["pid"] == r1["pid"]
            assert r2["permission"] == {"outcome": {"outcome": "selected", "optionId": "opt-once"}}
            assert r2["fs"]["code"] == -32601

            # 3. agent dies mid-turn → error with stderr tail, backend marked dead
            with pytest.raises(RuntimeError, match="agent crashed on purpose"):
                await backend.run("crash now", ctx)
            assert not backend.alive

            # 4. next turn restarts the process and opens a fresh session
            t4, ctx4 = await backend.run("hi again", ctx)
            r4 = _reply(t4)
            assert r4["pid"] != r1["pid"]
            assert r4["session"] != r1["session"]
            assert ctx4[f"{backend.session_ctx_key}_generation"] == 2
            return [r1["pid"], r4["pid"]]
        finally:
            await backend.close()

    pids = asyncio.run(scenario())
    assert wait_dead(pids) == []


def test_reject_policy_is_sent(tmp_path):
    backend = GrokAcpBackend(cwd=str(tmp_path), grok_cmd=str(FAKE_ACP), no_auto_update=False,
                             always_approve=False)
    assert backend.permission_policy == "reject"

    async def scenario():
        try:
            text, _ = await backend.run("permission", {})
            return _reply(text)
        finally:
            await backend.close()

    assert asyncio.run(scenario())["permission"] == {
        "outcome": {"outcome": "selected", "optionId": "opt-reject"}}


def test_invalid_policy_rejected():
    with pytest.raises(ValueError):
        GrokAcpBackend(permission_policy="sometimes")
