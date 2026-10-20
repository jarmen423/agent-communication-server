"""HeadlessCli hardening: timeout + process-group kill, non-zero exit, stderr
drain, `--` before positional prompts, streaming lines."""
from __future__ import annotations

import asyncio
import json
import sys

import pytest

from fixtures.helpers import BIN, MISBEHAVE, wait_dead, wait_for_file
from worker_backends.headless_cli import DEFAULT_TIMEOUT_SEC, HeadlessCliBackend, HeadlessCliSpec
from worker_backends.presets import kilo_spec, opencode_spec
from worker_backends.proc import run_streaming


def _backend(*args: str, **spec_kw) -> HeadlessCliBackend:
    spec_kw.setdefault("prompt_flag", None)
    return HeadlessCliBackend(
        HeadlessCliSpec(binary=sys.executable, base_argv=[str(MISBEHAVE), *args], **spec_kw)
    )


def test_default_timeout_is_900s():
    assert DEFAULT_TIMEOUT_SEC == 900.0
    assert HeadlessCliSpec(binary="x").timeout_sec == 900.0


@pytest.mark.parametrize("make_spec", [kilo_spec, opencode_spec])
def test_positional_presets_insert_end_of_options(make_spec):
    backend = HeadlessCliBackend(make_spec(repo="."))
    cmd = backend._build_cmd("--delete-everything", {})
    assert cmd[-2:] == ["--", "--delete-everything"]
    resumed = backend._build_cmd("next", {backend.spec.resume_ctx_key: "ses_1"})
    assert resumed[-4:] == ["--session", "ses_1", "--", "next"]


def test_flag_prompt_has_no_separator():
    backend = HeadlessCliBackend(HeadlessCliSpec(binary="agy", prompt_flag="-p"))
    assert backend._build_cmd("-x", {}) == ["agy", "-p", "-x"]


def test_dash_prompt_reaches_cli_as_positional():
    backend = _backend("echo-argv", end_of_options=True)
    text, _ = asyncio.run(backend.run("--help", {}))
    assert json.loads(text) == ["--", "--help"]


def test_timeout_kills_process_group(tmp_path):
    pidfile = tmp_path / "grandchild.pid"
    backend = _backend("sleep", str(pidfile), timeout_sec=1.0)

    async def run():
        task = asyncio.create_task(backend.run("ignored", {}))
        grandchild = int(await asyncio.to_thread(wait_for_file, pidfile))
        with pytest.raises(RuntimeError, match="timed out after 1s"):
            await task
        return grandchild

    grandchild = asyncio.run(run())
    assert wait_dead([grandchild], timeout=5) == [], "grandchild outlived the timeout kill"


def test_nonzero_exit_is_error_even_with_stdout():
    backend = _backend("nonzero")
    with pytest.raises(RuntimeError) as exc:
        asyncio.run(backend.run("p", {}))
    msg = str(exc.value)
    assert "exit 3" in msg
    assert "boom: something broke" in msg


def test_stderr_flood_does_not_deadlock():
    backend = _backend("noisy", timeout_sec=20)
    text, _ = asyncio.run(backend.run("p", {}))
    assert text == "ok"


def test_run_streaming_delivers_lines_and_bounds_stderr():
    seen: list[str] = []

    async def on_line(line: str) -> None:
        seen.append(line)

    async def run():
        return await run_streaming(
            [sys.executable, "-c",
             "import sys\nfor i in range(3): print(i, flush=True)\nsys.stderr.write('e'*50000)"],
            on_line=on_line, timeout=20, stderr_tail_bytes=1000,
        )

    res = asyncio.run(run())
    assert seen == ["0", "1", "2"]
    assert res.returncode == 0
    assert 0 < len(res.stderr_tail) <= 1000


def test_cancellation_kills_child(tmp_path):
    pidfile = tmp_path / "grandchild.pid"
    backend = _backend("sleep", str(pidfile), timeout_sec=60)

    async def run():
        task = asyncio.create_task(backend.run("p", {}))
        grandchild = int(await asyncio.to_thread(wait_for_file, pidfile))
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        return grandchild

    grandchild = asyncio.run(run())
    assert wait_dead([grandchild], timeout=5) == []


_HOST = """
import asyncio, sys
sys.path.insert(0, {repo!r})
from worker_backends.headless_cli import HeadlessCliBackend, HeadlessCliSpec
from worker_backends.proc import install_worker_signal_handlers
install_worker_signal_handlers()
b = HeadlessCliBackend(HeadlessCliSpec(binary=sys.executable, prompt_flag=None,
                                       base_argv=[{script!r}, "sleep", {pidfile!r}]))
try:
    asyncio.run(b.run("x", {{}}))
except KeyboardInterrupt:
    sys.exit(0)
"""


def test_sigterm_to_worker_stops_cli_group(tmp_path):
    """The CLI runs in its own session, so a supervisor's killpg() of the
    worker's group misses it; the worker's SIGTERM handler must forward it."""
    import signal
    import subprocess
    from pathlib import Path

    pidfile = tmp_path / "grandchild.pid"
    repo = str(Path(__file__).resolve().parents[2])
    host = subprocess.Popen([sys.executable, "-c", _HOST.format(
        repo=repo, script=str(MISBEHAVE), pidfile=str(pidfile))])
    try:
        grandchild = int(wait_for_file(pidfile, timeout=10))
        host.send_signal(signal.SIGTERM)
        assert host.wait(timeout=10) == 0
    finally:
        if host.poll() is None:
            host.kill()
    assert wait_dead([grandchild], timeout=5) == [], "CLI grandchild outlived its worker"


# ── Fake kilo/opencode CLIs (yargs-style parsing, NDJSON output) ─────


@pytest.mark.parametrize("name,make_spec", [("kilo", kilo_spec), ("opencode", opencode_spec)])
def test_fake_cli_dash_prompt_and_session_resume(tmp_path, monkeypatch, name, make_spec):
    """A prompt that looks like flags reaches the CLI as the message, and the
    session id from the NDJSON stream is resumed with --session."""
    argv_log = tmp_path / "argv.jsonl"
    monkeypatch.setenv("FAKE_CLI_ARGV_LOG", str(argv_log))
    spec = make_spec(repo=tmp_path, model="p/m", **{f"{name}_bin": str(BIN / name)})
    backend = HeadlessCliBackend(spec)

    text, ctx = asyncio.run(backend.run("-m evil --help", {}))
    assert text == "pong: -m evil --help"
    assert ctx[spec.resume_ctx_key] == f"ses_fake_{name}"
    text2, _ = asyncio.run(backend.run("--continue", ctx))
    assert text2 == "pong: --continue"

    first, second = (json.loads(line) for line in argv_log.read_text().splitlines())
    assert first[first.index("-m") + 1] == "p/m", "the model flag is the worker's, not the prompt's"
    assert first[-2:] == ["--", "-m evil --help"]
    assert second[-4:] == ["--session", f"ses_fake_{name}", "--", "--continue"]


def test_fake_cli_without_separator_rejects_dash_prompt(tmp_path):
    """Why `--` matters: without it yargs parses the prompt as flags."""
    spec = kilo_spec(repo=tmp_path, kilo_bin=str(BIN / "kilo"))
    spec.end_of_options = False
    with pytest.raises(RuntimeError, match="Unknown argument: --help"):
        asyncio.run(HeadlessCliBackend(spec).run("--help", {}))
