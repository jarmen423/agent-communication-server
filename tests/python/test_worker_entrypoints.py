"""Every worker type the supervisor advertises actually starts.

Each provider in ``worker_backends.providers.PROVIDER_CMDS`` is launched with
the exact argv ``worker_supervisor`` uses (``--identity --repo --nats-url``),
against fake CLIs/agents, and must register and answer a task per the reply
contract. ``cursor`` (needs the cursor-sdk package) and ``kilo-acp`` (needs a
running ``kilo acp --port`` server) are only required to start and to report
a clean terminal ``error`` result.
"""
from __future__ import annotations

import sys

import pytest

from fixtures.live import (
    REPO_ROOT,
    fake_env,
    live,
    run,
    send_task,
    start_worker,
    stop_worker,
    uniq,
    wait_result,
)
from worker_backends.providers import PROVIDER_CMDS, worker_argv

# provider → substring the fake's answer to "ping" contains (None = error expected)
EXPECTED = {
    "claude": "pong: ping",
    "codex": "pong: ping",
    "grok": "hello ",
    "hermes": "hello ",
    "echo": "echo: gnip",
    "agy": "pong: ping",
    "cursor": None,
    "kilo": "pong: ping",
    "kilo-acp": None,
    "opencode": "pong: ping",
    "opencode-acp": "hello ",
}


def test_every_advertised_provider_is_covered():
    assert set(EXPECTED) == set(PROVIDER_CMDS)
    for provider, cmd in PROVIDER_CMDS.items():
        assert (REPO_ROOT / cmd[0]).is_file(), provider


def _closed_port() -> int:
    import socket

    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


@live
@pytest.mark.parametrize("provider", sorted(PROVIDER_CMDS))
def test_provider_starts_and_answers(provider, tmp_path):
    import os

    import nats

    identity = uniq(f"ep-{provider}")
    url = os.environ["NATS_URL"]
    argv = worker_argv(provider, identity, python=sys.executable, script_dir=REPO_ROOT,
                       repo=tmp_path, nats_url=url)
    env = fake_env(CURSOR_API_KEY="dummy-key", KILO_ACP_PORT=str(_closed_port()))
    log = tmp_path / "worker.log"

    async def scenario():
        nc = await nats.connect(url)
        proc = await start_worker(nc, identity, argv, env, log)
        try:
            task_id, sub = await send_task(nc, "pytest", identity, "ping")
            return await wait_result(sub, task_id, within=60)
        finally:
            stop_worker(proc)
            await nc.close()

    result = run(scenario())
    payload = result["payload"]
    expected = EXPECTED[provider]
    if expected is None:
        assert payload["status"] == "error", payload
        assert payload["error"], payload
    else:
        assert payload["status"] == "done", f"{payload}\n{log.read_text()}"
        assert expected in payload["result"], payload
