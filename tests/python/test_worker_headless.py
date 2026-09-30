"""HeadlessCli hardening: timeout + process-group kill, non-zero exit, stderr
drain, `--` before positional prompts, streaming lines."""
from __future__ import annotations

import asyncio
import json
import sys

import pytest

from fixtures.helpers import MISBEHAVE, wait_dead, wait_for_file
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
