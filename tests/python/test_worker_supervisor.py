"""worker_supervisor: provider mapping, per-child logs, fast readiness,
bounded crash restarts, process-group cleanup. The live test runs the real
supervisor under with_stack.sh and SIGTERMs it."""
from __future__ import annotations

import asyncio
import json
import os
import signal
import subprocess
import sys
import time
import uuid
from pathlib import Path
from types import SimpleNamespace

import pytest

import worker_supervisor as ws
from fixtures.helpers import FAKE_WORKER, pid_alive, wait_dead, wait_for_file
from worker_backends.supervision import RestartPolicy, log_path_for

REPO_ROOT = Path(__file__).resolve().parents[2]


def test_claude_and_codex_map_to_real_workers():
    assert ws.PROVIDER_CMDS["claude"][0] == "claude_worker.py"
    assert ws.PROVIDER_CMDS["codex"][0] == "codex_worker.py"
    echo_users = [p for p, cmd in ws.PROVIDER_CMDS.items() if cmd[0] == "echo_worker.py"]
    assert echo_users == ["echo"]
    for cmd in ws.PROVIDER_CMDS.values():
        assert (REPO_ROOT / cmd[0]).is_file()


def test_cmd_argv(tmp_path):
    sup = ws.Supervisor("nats://x:1", tmp_path, "/py", log_dir=tmp_path)
    argv = sup._cmd("claude", "claude-7", model="sonnet")
    assert argv[:2] == ["/py", str(REPO_ROOT / "claude_worker.py")]
    assert argv[argv.index("--identity") + 1] == "claude-7"
    assert argv[argv.index("--repo") + 1] == str(tmp_path)
    assert argv[argv.index("--model") + 1] == "sonnet"
    with pytest.raises(ValueError):
        sup._cmd("nope", "x")


def test_log_path_is_sanitized(tmp_path):
    assert log_path_for(tmp_path, "../../etc/passwd").parent == tmp_path


def test_restart_policy_backoff_and_budget():
    pol = RestartPolicy(max_restarts=3, window_sec=100, base_backoff=1, max_backoff=3)
    times: list[float] = []
    delays = []
    for t in (0.0, 1.0, 2.0):
        delays.append(pol.next_delay(times, now=t))
        times.append(t)
    assert delays == [1, 2, 3]
    assert pol.next_delay(times, now=3.0) is None
    assert pol.next_delay(times, now=150.0) == 1  # window slid past old restarts


@pytest.fixture
def sup(tmp_path, monkeypatch):
    monkeypatch.setitem(ws.PROVIDER_CMDS, "fake", [FAKE_WORKER.name])
    s = ws.Supervisor("nats://unused:1", tmp_path, sys.executable, log_dir=tmp_path / "logs",
                      restart_policy=RestartPolicy(max_restarts=1, base_backoff=0.1),
                      script_dir=FAKE_WORKER.parent)
    yield s
    # Children were created in each test's own event loop: kill groups directly.
    for child in list(s.children.values()):
        try:
            os.killpg(child.proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def _run(coro):
    return asyncio.run(coro)


def test_spawn_is_ready_from_log_marker_and_stop_kills_group(sup, tmp_path, monkeypatch):
    pidfile = tmp_path / "gc.pid"
    monkeypatch.setitem(ws.PROVIDER_CMDS, "fake", [FAKE_WORKER.name, "--pidfile", str(pidfile)])

    async def scenario():
        t0 = time.monotonic()
        res = await sup._spawn("fake-1", "fake")
        elapsed = time.monotonic() - t0
        gc = int(await asyncio.to_thread(wait_for_file, pidfile))
        stop = await sup._stop("fake-1")
        return res, elapsed, gc, stop

    res, elapsed, grandchild, stop = _run(scenario())
    assert res["ok"] and res["status"] == "started" and res["ready"] is True
    assert elapsed < 10, "readiness should not wait for the 30s heartbeat"
    log = Path(res["log"])
    assert log == tmp_path / "logs" / "fake-1.log"
    assert "subscribed to channel.inbox.fake-1" in log.read_text()
    assert stop["status"] == "stopped"
    assert wait_dead([res["pid"], grandchild]) == []


def test_worker_that_dies_on_start_is_reported(sup, tmp_path, monkeypatch):
    counter = tmp_path / "count"
    monkeypatch.setitem(ws.PROVIDER_CMDS, "fake", [
        FAKE_WORKER.name, "--mode", "crash", "--crash-times", "9", "--counter", str(counter)])
    res = _run(sup._spawn("fake-2", "fake"))
    assert res["ok"] is False and res["error"] == "worker exited during start"
    assert "crashing (start #1)" in res["log_tail"]
    assert "fake-2" not in sup.children


def test_crash_restart_with_bounded_budget(sup):
    async def scenario():
        res = await sup._spawn("fake-3", "fake")
        first = res["pid"]
        os.killpg(first, signal.SIGKILL)
        await asyncio.sleep(0.3)
        sup.check_children()  # schedules a restart after 0.1s
        for _ in range(100):
            await asyncio.sleep(0.05)
            child = sup.children.get("fake-3")
            if child and child.proc.pid != first and child.alive:
                break
        child = sup.children["fake-3"]
        second = child.proc.pid
        assert second != first and child.restarts == 1
        # budget (max_restarts=1) is now spent: the next crash gives up
        os.killpg(second, signal.SIGKILL)
        await asyncio.sleep(0.3)
        sup.check_children()
        return first, second, child.log_path

    first, second, log = _run(scenario())
    assert "fake-3" not in sup.children
    assert log.read_text().count("==== ") == 2  # two spawn headers in one log
    assert wait_dead([first, second]) == []


def test_presence_heartbeat_marks_ready(sup, monkeypatch):
    monkeypatch.setitem(ws.PROVIDER_CMDS, "fake", [FAKE_WORKER.name, "--mode", "silent"])
    sup.ready_timeout = 5.0

    async def scenario():
        async def heartbeat():
            await asyncio.sleep(0.5)
            env = {"meta": {"from": "fake-4", "kind": "status"}, "payload": {"identity": "fake-4"}}
            await sup._on_presence(SimpleNamespace(data=json.dumps(env).encode()))

        hb = asyncio.create_task(heartbeat())
        res = await sup._spawn("fake-4", "fake")
        await hb
        return res

    res = _run(scenario())
    assert res["ready"] is True and res["status"] == "started"


def test_silent_worker_reports_pending_presence(sup, monkeypatch):
    monkeypatch.setitem(ws.PROVIDER_CMDS, "fake", [FAKE_WORKER.name, "--mode", "silent"])
    sup.ready_timeout = 0.5
    res = _run(sup._spawn("fake-5", "fake"))
    assert res["ok"] and res["status"] == "started_pending_presence"


live = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_TEST_STACK"),
    reason="needs scripts/dev/with_stack.sh (nats-server + hub-server)",
)


@live
def test_supervisor_process_ensure_and_sigterm_cleanup(tmp_path):
    import nats

    url = os.environ["NATS_URL"]
    log_dir = tmp_path / "workers"
    sup = subprocess.Popen(
        [sys.executable, "worker_supervisor.py", "--nats-url", url, "--log-dir", str(log_dir)],
        cwd=REPO_ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        env=dict(os.environ, PYTHONUNBUFFERED="1"),
    )
    identity = f"echo-{uuid.uuid4().hex[:6]}"

    async def ensure() -> dict:
        nc = await nats.connect(url)
        try:
            for _ in range(50):  # wait for the supervisor to subscribe
                try:
                    msg = await nc.request("hub.worker.list", b"{}", timeout=0.5)
                    break
                except Exception:
                    await asyncio.sleep(0.2)
            else:
                raise AssertionError("supervisor never answered hub.worker.list")
            assert json.loads(msg.data)["ok"]
            req = {"identity": identity, "provider": "echo"}
            msg = await nc.request("hub.worker.ensure", json.dumps(req).encode(), timeout=40)
            return json.loads(msg.data)
        finally:
            await nc.close()

    try:
        res = asyncio.run(ensure())
        assert res["ok"] and res["ready"] is True, res
        assert pid_alive(res["pid"])
        assert (log_dir / f"{identity}.log").is_file()
    finally:
        sup.send_signal(signal.SIGTERM)
        out, _ = sup.communicate(timeout=20)
        print(out.decode(errors="replace")[-2000:])
    assert wait_dead([res["pid"]], timeout=5) == [], "worker outlived the supervisor"
