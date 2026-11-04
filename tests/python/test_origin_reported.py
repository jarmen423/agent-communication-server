"""Clients report session and model in meta.origin.reported: env defaults, worker
backends (fake CLIs), the MCP server and its hook, the remote endpoint, and live
runs through the router."""
from __future__ import annotations

import asyncio
import json
import os
import stat
import subprocess
import sys
import textwrap
import time
import uuid
from pathlib import Path

import httpx
import pytest

import mcp_remote  # noqa: F401  (puts mcp_server/ on sys.path)
import hub_connection as conn
import hub_origin
from fixtures.helpers import BIN, FAKE_ACP
from fixtures.live import (REPO_ROOT, fake_env, live, run, send_task, start_worker, stop_worker,
                           uniq, wait_result)
from worker_backends import origin_report
from worker_backends.acp_agent import AcpAgentBackend, AcpAgentTransport
from worker_backends.claude_code import ClaudeCodeBackend, ClaudeCodeConfig
from worker_backends.codex_cli import CodexBackend, CodexConfig
from worker_backends.envelope import make_envelope
from worker_backends.headless_cli import HeadlessCliBackend, HeadlessCliSpec
from worker_backends.hermes_acp import HermesAcpBackend
from worker_backends.sdk_agent import SdkAgentBackend
from worker_events import execute_with_events, run_to_result

ENV_SESSION, ENV_MODEL, ENV_PROVIDER = (
    "NATS_HUB_REPORTED_SESSION", "NATS_HUB_REPORTED_MODEL", "NATS_HUB_REPORTED_MODEL_PROVIDER")


@pytest.fixture(autouse=True)
def clean_env(monkeypatch, tmp_path):
    for var in (ENV_SESSION, ENV_MODEL, ENV_PROVIDER, "CLAUDE_CODE_SESSION_ID"):
        monkeypatch.delenv(var, raising=False)
    monkeypatch.setenv("XDG_RUNTIME_DIR", str(tmp_path / "run"))
    (tmp_path / "run").mkdir()
    monkeypatch.setattr(hub_origin, "_client", None)
    monkeypatch.setattr(hub_origin, "_client_name", None)
    monkeypatch.setattr(hub_origin, "_host_pid_cache", None)


def reported_of(raw: bytes | dict) -> dict:
    env = json.loads(raw) if isinstance(raw, bytes) else raw
    return (env["meta"].get("origin") or {}).get("reported") or {}


# ── Environment contract ────────────────────────────────────────────────


def test_env_defaults_are_trimmed_and_empty_means_unset(monkeypatch):
    assert "origin" not in json.loads(make_envelope("a", None, "c", "message", {}))["meta"]
    monkeypatch.setenv(ENV_SESSION, "  s1 ")
    monkeypatch.setenv(ENV_MODEL, "   ")
    monkeypatch.setenv(ENV_PROVIDER, "p1")
    assert reported_of(make_envelope("a", None, "c", "message", {})) == {
        "session": "s1", "model_provider": "p1"}


def test_client_sends_over_cap_values_as_is(monkeypatch):
    long = "x" * 300
    monkeypatch.setenv(ENV_MODEL, long)
    assert reported_of(make_envelope("a", None, "c", "message", {})) == {"model": long}


def test_channel_entry_replaces_env_until_cleared(monkeypatch):
    monkeypatch.setenv(ENV_MODEL, "from-env")
    channel = uniq("c")
    origin_report.set_reported(channel, {"session": "s"})
    assert reported_of(make_envelope("a", None, channel, "message", {})) == {"session": "s"}
    assert reported_of(make_envelope("a", None, "other", "message", {})) == {"model": "from-env"}
    origin_report.clear_reported(channel)
    assert reported_of(make_envelope("a", None, channel, "message", {})) == {"model": "from-env"}


# ── Worker turns ────────────────────────────────────────────────────────


async def _turn(backend, channel, ctx=None, task_id="T", wave_channel=None):
    """One turn; every published envelope is built as the runtime builds it."""
    sent: list[tuple[str, dict]] = []

    async def publish(ch, kind, payload, reply_to=None):
        sent.append((ch, json.loads(make_envelope("w", None, ch, kind, payload, reply_to))))

    async def publish_event(ch, event_type, data, reply_to=None):
        await publish(ch, "event", {"event_type": event_type, "data": data}, reply_to)

    await execute_with_events(
        publish=publish, publish_event_fn=publish_event, backend=backend, channel=channel,
        prompt="ping", ctx=ctx or {}, task_id=task_id, working_status="working",
        done_status="done", wave_channel=wave_channel)
    return sent


def _result(sent):
    return next(env for _, env in sent if env["meta"]["kind"] == "message")


def test_claude_turn_reports_init_values_and_clears(tmp_path):
    backend = ClaudeCodeBackend(ClaudeCodeConfig(claude_bin=str(BIN / "claude"), repo=tmp_path))
    channel = uniq("task")
    sent = asyncio.run(_turn(backend, channel))
    assert reported_of(_result(sent)) == {"session": "fake-session-0001", "model": "fake-model"}
    assert reported_of(sent[-1][1]) == {"session": "fake-session-0001", "model": "fake-model"}
    assert reported_of(make_envelope("w", None, channel, "status", {})) == {}


def test_claude_init_model_beats_configured_alias(tmp_path):
    backend = ClaudeCodeBackend(ClaudeCodeConfig(
        claude_bin=str(BIN / "claude"), repo=tmp_path, model="opus"))
    sent = asyncio.run(_turn(backend, uniq("task")))
    assert reported_of(_result(sent))["model"] == "fake-model"


def test_codex_turn_reports_thread_and_configured_model(tmp_path):
    backend = CodexBackend(CodexConfig(
        codex_bin=str(BIN / "codex"), repo=tmp_path, model="gpt-x", skip_git_repo_check=True))
    sent = asyncio.run(_turn(backend, uniq("task")))
    assert reported_of(_result(sent)) == {"session": "thread-fake-0001", "model": "gpt-x"}


def test_codex_without_a_configured_model_leaves_it_out(tmp_path):
    backend = CodexBackend(CodexConfig(
        codex_bin=str(BIN / "codex"), repo=tmp_path, skip_git_repo_check=True))
    sent = asyncio.run(_turn(backend, uniq("task")))
    assert reported_of(_result(sent)) == {"session": "thread-fake-0001"}


def test_acp_turn_reports_session_and_clears_between_turns(tmp_path):
    async def go():
        backend = HermesAcpBackend(model="acp-model", cwd=str(tmp_path),
                                   hermes_cmd=str(FAKE_ACP))
        channel = uniq("session")
        try:
            first = await _turn(backend, channel, task_id="T1")
            between = reported_of(make_envelope("w", None, channel, "status", {}))
            second = await _turn(backend, channel, task_id="T2")
        finally:
            await backend.close()
        return first, between, second

    first, between, second = asyncio.run(go())
    one, two = reported_of(_result(first)), reported_of(_result(second))
    assert one["model"] == two["model"] == "acp-model"
    assert one["session"].startswith("s-") and two["session"].startswith("s-")
    assert one["session"] != two["session"]  # no ctx passed: a new ACP session
    assert between == {}


def test_runtime_values_override_env_key_by_key(tmp_path, monkeypatch):
    monkeypatch.setenv(ENV_MODEL, "env-model")
    monkeypatch.setenv(ENV_PROVIDER, "openrouter")
    backend = ClaudeCodeBackend(ClaudeCodeConfig(claude_bin=str(BIN / "claude"), repo=tmp_path))
    sent = asyncio.run(_turn(backend, uniq("task")))
    assert reported_of(sent[0][1]) == {"model": "env-model", "model_provider": "openrouter"}
    assert reported_of(_result(sent)) == {"session": "fake-session-0001", "model": "fake-model",
                                          "model_provider": "openrouter"}


def test_wave_mirror_reports_too_and_is_cleared(tmp_path):
    backend = ClaudeCodeBackend(ClaudeCodeConfig(claude_bin=str(BIN / "claude"), repo=tmp_path))
    channel, wave = uniq("session"), uniq("wave")
    sent = asyncio.run(_turn(backend, channel, wave_channel=wave))
    mirrored = [env for ch, env in sent if ch == wave]
    assert reported_of(mirrored[-1])["session"] == "fake-session-0001"
    assert reported_of(make_envelope("w", None, wave, "event", {})) == {}


def test_a_failed_turn_still_clears(tmp_path, monkeypatch):
    monkeypatch.setenv("FAKE_CLAUDE_MODE", "error")
    backend = ClaudeCodeBackend(ClaudeCodeConfig(claude_bin=str(BIN / "claude"), repo=tmp_path))
    channel = uniq("task")
    sent = asyncio.run(_turn(backend, channel))
    result = _result(sent)
    assert result["payload"]["status"] == "error"
    assert reported_of(result)["session"] == "fake-session-0001"
    assert reported_of(make_envelope("w", None, channel, "status", {})) == {}


def test_status_without_a_message_is_recorded_not_published(tmp_path):
    class Silent:
        async def run(self, prompt, ctx):
            from worker_backends.progress import current_progress_handler

            await origin_report.report_status(current_progress_handler(), session="s9")
            return "ok", ctx

    sent = asyncio.run(_turn(Silent(), uniq("task")))
    progress = [env["payload"]["data"] for _, env in sent
                if env["payload"].get("event_type") == "progress"]
    assert all("session_id" not in data for data in progress)
    assert reported_of(_result(sent)) == {"session": "s9"}


def test_plain_dm_reply_reports_the_turns_values(tmp_path):
    backend = ClaudeCodeBackend(ClaudeCodeConfig(claude_bin=str(BIN / "claude"), repo=tmp_path))

    async def go():
        await run_to_result(backend, "ping", {}, "task-1")
        return (make_envelope("w", "bob", "inbox.bob", "message", {}, reply_to="task-1"),
                make_envelope("w", "bob", "inbox.bob", "message", {}, reply_to="other"))

    answer, unrelated = asyncio.run(go())
    assert reported_of(answer) == {"session": "fake-session-0001", "model": "fake-model"}
    assert reported_of(unrelated) == {}


def test_headless_reads_configured_values_and_parsed_session(tmp_path):
    script = tmp_path / "cli"
    script.write_text("#!/bin/sh\necho 'session_id: h-1'\necho hello\n")
    script.chmod(0o755)
    backend = HeadlessCliBackend(HeadlessCliSpec(
        binary=str(script), repo=tmp_path, base_argv=["-m", "m1", "--provider=openrouter"],
        resume_mode="resume_id", resume_ctx_key="hid", strip_line_prefixes=("session_id:",)))
    sent = asyncio.run(_turn(backend, uniq("task")))
    assert reported_of(_result(sent)) == {
        "session": "h-1", "model": "m1", "model_provider": "openrouter"}


def test_headless_with_nothing_known_reports_nothing(tmp_path):
    backend = HeadlessCliBackend(HeadlessCliSpec(binary="/bin/echo", repo=tmp_path))
    assert reported_of(_result(asyncio.run(_turn(backend, uniq("task"))))) == {}


def test_a_failing_progress_handler_never_breaks_a_report():
    async def boom(kind, data):
        raise RuntimeError("nope")

    asyncio.run(origin_report.report_status(boom, session="s"))


def test_acp_agent_and_sdk_backends_report():
    class Transport(AcpAgentTransport):
        model = "t-model"

        async def send_turn(self, prompt, session_handle):
            return "ok", "handle-1"

    sent = asyncio.run(_turn(AcpAgentBackend(Transport()), uniq("task")))
    assert reported_of(_result(sent)) == {"session": "handle-1", "model": "t-model"}

    sdk = SdkAgentBackend(lambda prompt, ctx: ("ok", {**ctx, "agent_id": "a-1"}), model="m2")
    sent = asyncio.run(_turn(sdk, uniq("task")))
    assert reported_of(_result(sent)) == {"session": "a-1", "model": "m2"}


# ── MCP server: environment, clientInfo, host session ───────────────────


def test_mcp_envelope_sources_in_order(monkeypatch):
    monkeypatch.setenv("NATS_HUB_IDENTITY", "orch")
    assert "origin" not in conn.envelope("c", {})["meta"]

    hub_origin.note_client_info({"name": "claude-code", "version": "2.1.0"})
    assert reported_of(conn.envelope("c", {})) == {"client": "claude-code 2.1.0"}

    assert hub_origin.record_session("hook-session", pid=hub_origin.host_pid())
    assert reported_of(conn.envelope("c", {}))["session"] == "hook-session"

    monkeypatch.setenv(ENV_SESSION, "env-session")
    monkeypatch.setenv(ENV_MODEL, "env-model")
    assert reported_of(conn.envelope("c", {})) == {
        "client": "claude-code 2.1.0", "session": "env-session", "model": "env-model"}


def test_mcp_rereads_the_session_per_envelope(monkeypatch):
    monkeypatch.setenv("NATS_HUB_IDENTITY", "orch")
    pid = hub_origin.host_pid()
    hub_origin.record_session("first", pid=pid)
    assert reported_of(conn.envelope("c", {}))["session"] == "first"
    hub_origin.record_session("after-clear", pid=pid)
    assert reported_of(conn.envelope("c", {}))["session"] == "after-clear"


def test_claude_session_env_only_counts_for_a_claude_client(monkeypatch):
    monkeypatch.setenv("NATS_HUB_IDENTITY", "orch")
    monkeypatch.setenv("CLAUDE_CODE_SESSION_ID", "inherited")
    assert "origin" not in conn.envelope("c", {})["meta"]
    hub_origin.note_client_info({"name": "codex-mcp-client", "version": "1"})
    assert "session" not in reported_of(conn.envelope("c", {}))
    hub_origin.note_client_info({"name": "claude-code", "version": "2"})
    assert reported_of(conn.envelope("c", {}))["session"] == "inherited"


def test_a_bound_call_ignores_process_wide_sources(monkeypatch):
    monkeypatch.setenv(ENV_MODEL, "endpoint-env")
    hub_origin.note_client_info({"name": "x", "version": "1"})

    async def nc():
        return None

    with conn.bound("claude-web-1", nc):
        assert "origin" not in conn.envelope("c", {})["meta"]
        with hub_origin.scoped({"session": "conn-1"}):
            assert reported_of(conn.envelope("c", {})) == {"session": "conn-1"}


def test_client_info_bind_never_raises():
    class NoContext:
        @property
        def request_context(self):
            raise LookupError("no request")

    async def go():
        async with hub_origin.client_info_bind(NoContext()):
            return True

    assert asyncio.run(go())


def test_session_file_is_private(tmp_path):
    pid = os.getpid()
    assert hub_origin.record_session("s1", pid=pid)
    directory = tmp_path / "run" / "nats-hub"
    assert stat.S_IMODE(directory.stat().st_mode) == 0o700
    assert stat.S_IMODE((directory / f"session-{pid}.json").stat().st_mode) == 0o600
    assert hub_origin.read_session(pid=pid) == "s1"


def test_session_file_of_a_reused_pid_is_ignored(tmp_path):
    pid = os.getpid()
    hub_origin.record_session("old-host", pid=pid)
    path = tmp_path / "run" / "nats-hub" / f"session-{pid}.json"
    data = json.loads(path.read_text())
    data["host_start"] = "1"  # as if an earlier process with this pid wrote it
    path.write_text(json.dumps(data))
    assert hub_origin.read_session(pid=pid) is None
    assert not path.exists()


def test_session_dir_that_is_not_private_is_refused(tmp_path):
    directory = tmp_path / "run" / "nats-hub"
    directory.mkdir(mode=0o755)
    directory.chmod(0o755)
    assert not hub_origin.record_session("s1", pid=1)
    (directory / "session-1.json").write_text('{"session_id": "planted"}')
    assert hub_origin.read_session(pid=1) is None


def test_session_file_symlink_is_not_followed(tmp_path):
    hub_origin.record_session("s1", pid=4242)
    directory = tmp_path / "run" / "nats-hub"
    other = tmp_path / "other.json"
    other.write_text('{"session_id": "planted"}')
    (directory / "session-77.json").symlink_to(other)
    assert hub_origin.read_session(pid=77) is None


def test_recording_prunes_files_of_dead_hosts(tmp_path):
    proc = subprocess.Popen(["true"])
    proc.wait()
    hub_origin.record_session("gone", pid=proc.pid)
    hub_origin.record_session("here", pid=os.getpid())
    assert hub_origin.read_session(pid=proc.pid) is None
    assert hub_origin.read_session(pid=os.getpid()) == "here"


def test_bad_session_values_are_not_recorded():
    for bad in (None, 5, "", "   "):
        assert not hub_origin.record_session(bad, pid=4242)


_PRINT_HOST = textwrap.dedent("""\
    import sys
    sys.path.insert(0, sys.argv[1])
    import hub_origin
    print(hub_origin.host_pid(hook=sys.argv[2] == "hook"))
""")


def test_hook_and_server_name_the_same_host(tmp_path):
    """The host spawns an MCP server directly and a hook through ``sh -c``."""
    script = tmp_path / "print_host.py"
    script.write_text(_PRINT_HOST)
    server_dir = str(REPO_ROOT / "mcp_server")
    direct = subprocess.run([sys.executable, str(script), server_dir, "server"],
                            capture_output=True, text=True, check=True).stdout.strip()
    # The trailing `:` keeps sh alive instead of letting it exec the command.
    via_shell = subprocess.run(
        ["/bin/sh", "-c", f'"{sys.executable}" "{script}" "{server_dir}" hook; :'],
        capture_output=True, text=True, check=True).stdout.strip()
    assert direct == via_shell == str(os.getpid())


@pytest.mark.parametrize("hook_input,expected", [
    (json.dumps({"session_id": "from-hook", "source": "startup"}), "from-hook"),
    (json.dumps({"source": "startup"}), None),
    ("not json", None),
    ("[1, 2]", None),
    ("", None),
])
def test_hook_records_the_session_and_always_succeeds(hook_input, expected, tmp_path):
    env = dict(os.environ, NATS_URL="nats://127.0.0.1:1", XDG_RUNTIME_DIR=str(tmp_path / "run"))
    proc = subprocess.run(
        ["/bin/sh", "-c",
         f'"{sys.executable}" "{REPO_ROOT / "mcp_server/hooks/session_start.py"}"; echo "$$" >&2'],
        input=hook_input, capture_output=True, text=True, env=env, timeout=30)
    assert proc.returncode == 0, proc.stderr
    context = json.loads(proc.stdout)["hookSpecificOutput"]["additionalContext"]
    assert context.startswith("[nats-hub]")
    # sh is the hook's parent, so the host it recorded for is this test process.
    assert hub_origin.read_session(pid=os.getpid()) == expected


def test_hook_survives_an_unusable_runtime_dir(tmp_path):
    blocker = tmp_path / "file"
    blocker.write_text("")
    env = dict(os.environ, NATS_URL="nats://127.0.0.1:1", XDG_RUNTIME_DIR=str(blocker))
    proc = subprocess.run(
        [sys.executable, str(REPO_ROOT / "mcp_server/hooks/session_start.py")],
        input=json.dumps({"session_id": "s"}), capture_output=True, text=True, env=env,
        timeout=30)
    assert proc.returncode == 0 and json.loads(proc.stdout)


# ── Live, through the router ────────────────────────────────────────────


@live
@pytest.mark.parametrize("argv,expected", [
    (["claude_worker.py"], {"session": "fake-session-0001", "model": "fake-model"}),
    (["codex_worker.py", "--model", "gpt-x"], {"session": "thread-fake-0001", "model": "gpt-x"}),
    (["hermes_acp_worker.py", "--model", "acp-model"], {"model": "acp-model"}),
], ids=["claude", "codex", "acp"])
def test_worker_results_carry_reported_values(argv, expected, tmp_path):
    import nats

    async def go():
        url = os.environ["NATS_URL"]
        identity, sender = uniq("reporter"), uniq("sender")
        nc = await nats.connect(url)
        worker = None
        try:
            worker = await start_worker(
                nc, identity,
                [sys.executable, *argv, "--identity", identity, "--nats-url", url,
                 *(["--repo", str(tmp_path), "--skip-git-repo-check"] if "codex" in argv[0]
                   else ["--repo", str(tmp_path)] if "claude" in argv[0] else [])],
                fake_env(NATS_HUB_REPORTED_MODEL_PROVIDER="env-provider"),
                tmp_path / "worker.log")
            task_id, sub = await send_task(nc, sender, identity, "ping")
            return await wait_result(sub, task_id)
        finally:
            if worker is not None:
                stop_worker(worker)
            await nc.close()

    result = run(go())
    assert result["payload"]["status"] == "done", result
    reported = reported_of(result)
    assert reported["model_provider"] == "env-provider"
    for key, value in expected.items():
        assert reported[key] == value, reported
    assert reported["session"], reported
    assert result["meta"]["origin"]["identity"], result  # the router's part is there too


@live
def test_over_cap_values_are_dropped_by_the_hub(monkeypatch):
    import nats

    long = "x" * 300
    monkeypatch.setenv(ENV_MODEL, long)
    monkeypatch.setenv(ENV_SESSION, "kept")

    async def go():
        nc = await nats.connect(os.environ["NATS_URL"])
        try:
            identity, channel = uniq("capped"), uniq("capped")
            sub = await nc.subscribe(f"channel.{channel}")
            await nc.flush()
            body = make_envelope(identity, None, channel, "message", {})
            assert reported_of(body)["model"] == long  # the client doesn't cap
            await nc.publish(f"hub.pub.{identity}.{channel}", body)
            await nc.flush()
            return json.loads((await sub.next_msg(timeout=10)).data)
        finally:
            await nc.close()

    assert reported_of(run(go())) == {"session": "kept"}


@live
def test_rust_cli_reports_env_and_history_filters_on_it():
    """hub-delegate reports through the environment; history.query model=x finds it."""
    import nats

    binary = REPO_ROOT / "target" / "debug" / "hub-delegate"
    assert binary.exists(), f"build first: {binary}"
    sender, model = uniq("cli"), f"model-{uuid.uuid4().hex[:8]}"
    env = dict(os.environ, NATS_HUB_REPORTED_MODEL=model, NATS_HUB_REPORTED_SESSION="cli-session")
    subprocess.run([str(binary), "--to", uniq("nobody"), "--from", sender, "--prompt", "hi",
                    "--no-wait"], env=env, check=True, capture_output=True, timeout=30)

    async def go():
        nc = await nats.connect(os.environ["NATS_URL"])
        try:
            request = json.dumps({"op": "history.query",
                                  "params": {"model": model, "limit": 5}}).encode()
            deadline = time.monotonic() + 15
            while True:
                reply = json.loads((await nc.request(
                    f"hub.api.{sender}.history.query", request, timeout=5)).data)
                assert reply.get("ok"), reply
                rows = reply["data"]["envelopes"]
                if rows or time.monotonic() > deadline:
                    return reply["data"]
                await asyncio.sleep(0.2)
        finally:
            await nc.close()

    data = run(go())
    assert "model" in data["applied_filters"]
    assert len(data["envelopes"]) == 1, data
    row = data["envelopes"][0]
    assert row["from_identity"] == sender
    assert row["origin"]["reported"] == {"model": model, "session": "cli-session"}
