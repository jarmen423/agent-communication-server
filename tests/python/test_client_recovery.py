"""Connection recovery and permanent-close behavior at the public Python boundary."""
import asyncio
import subprocess
import sys
from pathlib import Path
from unittest.mock import AsyncMock

import pytest
from nats.aio.client import Client

from nats_connect import _RefreshingCredentialsClient
import worker_runtime


@pytest.mark.parametrize('reason', [
    'user authentication expired', 'maximum account active connections exceeded',
    'authorization violation', 'user authentication revoked',
])
def test_refreshable_session_errors_reconnect(reason):
    async def run():
        client = _RefreshingCredentialsClient()
        client._status = Client.CONNECTED
        client.options['allow_reconnect'] = True
        client._process_op_err = AsyncMock()
        await client._process_err(f"'{reason}'")
        client._process_op_err.assert_awaited_once()
    asyncio.run(run())


def test_permission_errors_remain_nonfatal_and_reported():
    async def run():
        client = _RefreshingCredentialsClient()
        client._status = Client.CONNECTED
        client.options['allow_reconnect'] = True
        client._error_cb = AsyncMock()
        for reason in ('permissions violation for publish to "secret"',
                       'permissions violation for subscription to "secret"'):
            await client._process_err(reason)
        assert client.is_connected
        assert client._error_cb.await_count == 2
    asyncio.run(run())


def test_worker_permanent_close_raises_for_nonzero_exit(monkeypatch, capsys):
    async def run():
        ready = asyncio.Event()
        client = AsyncMock()
        client.is_closed = False
        subscription = AsyncMock()
        subscription.durable = False
        callbacks = {}

        async def connect(*args, **kwargs):
            callbacks.update(kwargs)
            return client

        async def flush():
            ready.set()

        client.flush.side_effect = flush
        monkeypatch.setattr(worker_runtime, 'connect_nats', connect)
        monkeypatch.setattr(worker_runtime.hub_delivery, 'subscribe_inbox',
                            AsyncMock(return_value=subscription))
        monkeypatch.setattr(worker_runtime, 'install_stop_signals', lambda _: None)
        cfg = worker_runtime.WorkerConfig(identity='worker-close', backend=AsyncMock(),
                                          session_resume=False)
        worker = asyncio.create_task(worker_runtime.run_worker(cfg))
        try:
            await asyncio.wait_for(ready.wait(), 1)
            client.is_closed = True
            if 'closed_cb' in callbacks:
                await callbacks['closed_cb']()
            with pytest.raises(ConnectionError, match='NATS connection closed'):
                await asyncio.wait_for(asyncio.shield(worker), 1)
            subscription.unsubscribe.assert_awaited_once()
            client.close.assert_awaited_once()
        finally:
            worker.cancel()
            await asyncio.gather(worker, return_exceptions=True)
    asyncio.run(run())
    assert 'NATS connection closed' in capsys.readouterr().err


def test_credential_backoff_is_jittered_and_capped():
    from types import SimpleNamespace
    from nats_connect import _credential_reconnect

    delays = []
    for attempts in (0, 1, 2, 3, 10, 1000):
        server = SimpleNamespace(reconnects=attempts)
        samples = [_credential_reconnect([server], {}) for _ in range(10)]
        assert all(selected is server for selected, _ in samples)
        cap = min(30, 2 ** min(attempts + 1, 5))
        assert all(cap / 2 <= delay <= cap for _, delay in samples)
        assert len({delay for _, delay in samples}) > 1
        delays.append(samples[0][1])
    assert delays[-1] <= 30


def test_worker_closed_connection_exits_nonzero():
    script = """
import asyncio
from unittest.mock import AsyncMock
import worker_runtime as runtime

nc = AsyncMock()
nc.is_closed = True
sub = AsyncMock()
sub.durable = False
runtime.connect_nats = AsyncMock(return_value=nc)
runtime.hub_delivery.subscribe_inbox = AsyncMock(return_value=sub)
asyncio.run(runtime.run_worker(runtime.WorkerConfig(
    identity='closed-worker', backend=AsyncMock(), session_resume=False)))
"""
    process = subprocess.run([sys.executable, '-c', script],
        cwd=Path(__file__).resolve().parents[2], text=True, capture_output=True, timeout=5)
    assert process.returncode == 1, process.stdout + process.stderr
    assert 'NATS connection closed permanently; exiting for restart' in process.stderr
