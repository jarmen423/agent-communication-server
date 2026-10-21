"""H-1c real operator-stack journey; prints a secret-free review transcript.

scripts/dev/operator_stack.sh .venv/bin/python -m pytest -q -s tests/python/test_acs_e2e.py
"""
import asyncio
import json
import os
from pathlib import Path
import subprocess
import sys
import time

import httpx
import nats
import pytest
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from nats_connect import connect_nats
from stub_control_plane.nats_issuer import decode_jwt
from test_acs_cli import ACS, REPO, acs, free_port, headless_login

pytestmark = pytest.mark.skipif(not os.environ.get('NATS_HUB_OPERATOR_TEST_STACK'),
                                reason='requires scripts/dev/operator_stack.sh')


def test_login_mcp_delegate_daemon_refresh_and_revoke(tmp_path):
    async def run():
        port = free_port()
        server = f'http://127.0.0.1:{port}'
        root = tmp_path / 'acs'
        home = tmp_path / 'client-home'
        op = Path(os.environ['OPERATOR_DIR'])
        env = {**{k: v for k, v in os.environ.items() if not k.startswith('NATS_')},
               'NATS_URL': os.environ['NATS_URL'], 'ACS_HOME': str(root),
               'ACS_NO_KEYRING': '1', 'ACS_SERVER': server, 'PYTHONUNBUFFERED': '1'}
        procs, logs = [], []

        def spawn(name, args, extra=None, pipe=False):
            logfile = (tmp_path / f'{name}.log').open('w')
            logs.append(logfile)
            p = subprocess.Popen(args, env={**env, **(extra or {})}, cwd=REPO,
                                 stdout=subprocess.PIPE if pipe else logfile,
                                 stderr=subprocess.PIPE if pipe else subprocess.STDOUT, text=True)
            procs.append(p)
            return p

        bob = None
        try:
            cp = spawn('stub', [sys.executable, '-m', 'stub_control_plane', '--port', str(port),
                                '--resource', server + '/mcp', '--nsc-home', str(op / 'nsc'),
                                '--tenant', 'T_A', '--service-key-file', str(tmp_path / 'admin.key'),
                                '--nats-url', os.environ['NATS_URL'], '--nats-ttl', '60', '--access-ttl', '30'])
            for _ in range(100):
                assert cp.poll() is None
                try:
                    if (await asyncio.to_thread(httpx.get, server + '/healthz')).status_code == 200:
                        break
                except httpx.HTTPError:
                    pass
                await asyncio.sleep(.1)
            else:
                raise TimeoutError('control plane startup')
            print('DEV ONLY control plane + operator stack: tenant T_A, durable bound hub-server', flush=True)
            login = await asyncio.to_thread(headless_login, env)
            print('$ acs login --headless\n' + login, end='', flush=True)
            print('$ acs agent add me\n' + (await asyncio.to_thread(acs, env, 'agent', 'add', 'me')).stdout, end='', flush=True)
            await asyncio.to_thread(acs, env, 'agent', 'add', 'device-echo', '--role', 'worker')
            spawn('echo', [str(ACS), 'worker', 'run', 'echo', '--identity', 'device-echo'])
            await asyncio.sleep(2)
            install = await asyncio.to_thread(acs, env, 'mcp', 'install', 'claude', '--identity', 'me', '--home', str(home))
            print('$ acs mcp install claude --identity me --home <tmp>\n' + install.stdout, end='', flush=True)
            config = json.loads((home / '.claude.json').read_text())['mcpServers']['nats-hub']
            assert set(config['env']) == {'NATS_URL', 'NATS_CREDENTIALS_FILE', 'NATS_HUB_IDENTITY'}
            # Start exactly the executable/arguments/environment written by acs.
            params = StdioServerParameters(command=config['command'], args=config['args'], env={**env, **config['env']})
            async with stdio_client(params) as (read, write):
                async with ClientSession(read, write) as session:
                    await session.initialize()
                    result = await session.call_tool('delegate_task', {'to': 'device-echo', 'prompt': 'hello acs', 'timeout': 20})
                    assert not result.isError, result
                    data = json.loads(result.content[0].text)
                    assert data['ok'] and data['data']['reply']['result'] == 'echo: sca olleh', data
                    print('MCP delegate_task → device-echo: echo: sca olleh', flush=True)
                    cred = root / 'creds/me.creds'
                    original = cred.read_text()
                    old = tmp_path / 'unrefreshed.creds'
                    old.write_text(original)
                    old.chmod(0o600)
                    before = decode_jwt(original.splitlines()[1])
                    observer = spawn('rust-client', [str(REPO / 'target/debug/hub-observe'),
                        '--nats-url', os.environ['NATS_URL'], '--identity', 'me',
                        '--channel', 'task.acsprobe', '--json'],
                        {**config['env'], 'RUST_LOG': 'off'}, pipe=True)
                    listening = await asyncio.wait_for(asyncio.to_thread(observer.stderr.readline), 10)
                    assert 'listening' in listening, listening
                    bob = await connect_nats(os.environ['NATS_URL'],
                        credentials_file=str(op / 'creds/T_A/bob.creds'), identity='bob')

                    async def rust_receives(marker):
                        envelope = {'meta': {'id': 'probe-' + str(time.time_ns()), 'from': 'bob', 'to': None,
                            'channel': 'task.acsprobe', 'kind': 'message', 'reply_to': None,
                            'timestamp': '2026-10-21T00:00:00Z'}, 'payload': {'marker': marker}}
                        await bob.publish('hub.pub.bob.task.acsprobe', json.dumps(envelope).encode())
                        await bob.flush()
                        line = await asyncio.wait_for(asyncio.to_thread(observer.stdout.readline), 10)
                        assert json.loads(line)['payload']['marker'] == marker

                    await rust_receives('before')
                    daemon = spawn('daemon', [str(ACS), 'up', '--foreground'])
                    print('$ acs up --foreground\nWaiting for actual 60-second JWT expiry; Rust HubClient and stdio MCP stay connected', flush=True)
                    await asyncio.sleep(max(0, before['exp'] + 4 - time.time()))
                    assert daemon.poll() is None and observer.poll() is None
                    after = decode_jwt(cred.read_text().splitlines()[1])
                    assert after['exp'] > before['exp'] and after['sub'] == before['sub']
                    await rust_receives('after')
                    print('PASS same Rust HubClient receives before/after expiry; refreshed JWT retains local nkey', flush=True)
                    result = await session.call_tool('delegate_task', {'to': 'device-echo', 'prompt': 'after refresh', 'timeout': 20})
                    assert not result.isError, result
                    data = json.loads(result.content[0].text)
                    assert data['data']['reply']['result'] == 'echo: hserfer retfa', data
                    print('PASS same stdio MCP + Python worker after daemon refresh: echo: hserfer retfa', flush=True)
                    async def quiet(_):
                        pass
                    with pytest.raises(nats.errors.Error):
                        await nats.connect(os.environ['NATS_URL'], user_credentials=str(old), allow_reconnect=False,
                                           connect_timeout=1, error_cb=quiet)
                    print('PASS unrefreshed copy rejected by NATS after expiry', flush=True)
                    state = json.loads((root / 'state.json').read_text())
                    refresh_args = {'identity': 'me', 'public_key': state['agents']['me']['public_key']}
                    token_headers = {'Authorization': 'Bearer ' + state['access_token']}
                    removed = await asyncio.to_thread(acs, env, 'agent', 'rm', 'me')
                    print('$ acs agent rm me\n' + removed.stdout, end='', flush=True)
                    denied = await asyncio.to_thread(httpx.post, server + '/v1/credentials/nats',
                                                     headers=token_headers, json=refresh_args)
                    assert denied.status_code == 403, denied.text
                    await asyncio.sleep(2)
                    assert not cred.exists() and 'me' not in json.loads((root / 'state.json').read_text())['agents']
                    print('PASS revoked identity cannot refresh; daemon does not recreate its credential', flush=True)
            stopped = await asyncio.to_thread(acs, env, 'down')
            print('$ acs down\n' + stopped.stdout, end='', flush=True)
            print('$ acs logout\n' + (await asyncio.to_thread(acs, env, 'logout')).stdout, end='', flush=True)
        finally:
            if bob:
                await bob.drain()
            for p in reversed(procs):
                if p.poll() is None:
                    p.terminate()
            for p in reversed(procs):
                try:
                    await asyncio.to_thread(p.wait, 10)
                except subprocess.TimeoutExpired:
                    p.kill()
                    await asyncio.to_thread(p.wait)
            for logfile in logs:
                logfile.close()
    asyncio.run(run())
