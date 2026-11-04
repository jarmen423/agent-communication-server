"""H-1c real operator-stack journey; prints a secret-free review transcript.

scripts/dev/operator_stack.sh .venv/bin/python -m pytest -q -s tests/python/test_acs_e2e.py
"""
import asyncio
import json
import os
import shutil
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
        # Workers use the runtime built into acs, as from a release install:
        # no ACS_WORKER_DIR, no ACS_PYTHON (acs builds its private venv) and
        # a working directory outside the checkout.
        env = {**{k: v for k, v in os.environ.items() if not k.startswith(('NATS_', 'ACS_', 'PYTHONPATH'))},
               'NATS_URL': os.environ['NATS_URL'], 'ACS_HOME': str(root),
               'ACS_NO_KEYRING': '1', 'ACS_DEVICE_NAME': 'integration laptop', 'ACS_SERVER': server, 'PYTHONUNBUFFERED': '1',
               'ACS_MCP_SERVER': str(REPO / 'mcp_server/nats_hub_mcp.py')}
        procs, logs = [], []
        away = tmp_path / 'elsewhere'
        away.mkdir()

        def spawn(name, args, extra=None, pipe=False, cwd=REPO):
            logfile = (tmp_path / f'{name}.log').open('w')
            logs.append(logfile)
            p = subprocess.Popen(args, env={**env, **(extra or {})}, cwd=cwd,
                                 stdout=subprocess.PIPE if pipe else logfile,
                                 stderr=subprocess.PIPE if pipe else subprocess.STDOUT, text=True)
            procs.append(p)
            return p

        bob = None
        try:
            cp = spawn('stub', [sys.executable, '-m', 'stub_control_plane', '--port', str(port),
                                '--resource', server + '/mcp', '--nsc-home', str(op / 'nsc'),
                                '--tenant', 'T_A', '--member-tenant', 'T_B', '--service-key-file', str(tmp_path / 'admin.key'),
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
            # Upgrade a device directory in the previous release's layout.
            device = json.loads((root / 'device.json').read_text())
            legacy = {**device, **json.loads((root / 'tenants/T_A/state.json').read_text())}
            legacy.pop('tenants')
            shutil.copytree(root / 'tenants/T_A/creds', root / 'creds')
            shutil.rmtree(root / 'tenants')
            (root / 'device.json').unlink()
            (root / 'current').unlink()
            (root / 'state.json').write_text(json.dumps(legacy))
            await asyncio.to_thread(acs, env, 'status')
            assert (root / 'state.json.bak').exists()
            assert (root / 'creds/me.creds').is_symlink()
            await asyncio.to_thread(acs, env, 'tenant', 'list')
            print('PASS previous device layout migrated; compatibility credential path retained', flush=True)
            await asyncio.to_thread(acs, env, 'agent', 'add', 'me', '--tenant', 'T_B')
            a = root / 'tenants/T_A/creds/me.creds'
            b = root / 'tenants/T_B/creds/me.creds'
            assert a.read_bytes() != b.read_bytes()
            b_original = b.read_bytes()
            # Connect both credentials to the same NATS listener: account isolation
            # must prevent A's publication from reaching B's subscription.
            ca = await nats.connect(os.environ['NATS_URL'], user_credentials=str(a))
            cb = await nats.connect(os.environ['NATS_URL'], user_credentials=str(b))
            sa = await ca.subscribe('channel.task.tenant-isolation')
            sb = await cb.subscribe('channel.task.tenant-isolation')
            await ca.flush()
            await cb.flush()
            await ca.publish('hub.pub.me.task.tenant-isolation', json.dumps({
                'meta': {'id': 'isolated', 'from': 'me', 'to': None,
                         'channel': 'task.tenant-isolation', 'kind': 'message',
                         'reply_to': None, 'timestamp': '2026-11-04T00:00:00Z'},
                'payload': {'probe': True}}).encode())
            await ca.flush()
            assert json.loads((await sa.next_msg(timeout=5)).data)['payload']['probe']
            with pytest.raises(nats.errors.TimeoutError):
                await sb.next_msg(timeout=.5)
            await cb.publish('hub.pub.me.task.tenant-isolation', json.dumps({
                'meta': {'id': 'b-probe', 'from': 'me', 'to': None,
                         'channel': 'task.tenant-isolation', 'kind': 'message',
                         'reply_to': None, 'timestamp': '2026-11-04T00:00:00Z'},
                'payload': {'tenant': 'B'}}).encode())
            await cb.flush()
            assert json.loads((await sb.next_msg(timeout=5)).data)['payload']['tenant'] == 'B'
            with pytest.raises(nats.errors.TimeoutError):
                await sa.next_msg(timeout=.5)
            await ca.drain()
            await cb.drain()
            print('PASS same identity has distinct credentials; A cannot publish into B account', flush=True)
            await asyncio.to_thread(acs, env, 'agent', 'add', 'only-a')
            await asyncio.to_thread(acs, env, 'env', '--tenant', 'T_B', 'only-a', ok=False)
            await asyncio.to_thread(acs, env, 'exec', '--tenant', 'T_B', 'only-a', '--', 'true', ok=False)
            child = await asyncio.to_thread(acs, env, 'exec', '--tenant', 'T_B', 'me', '--',
                sys.executable, '-c', 'import os; print(os.environ["NATS_CREDENTIALS_FILE"])')
            assert child.stdout.strip() == str(b)
            # Leave a revoked identity in A's local state: its renewal fails,
            # while the daemon must continue renewing B's identities.
            token = json.loads((root / 'device.json').read_text())['access_token']
            denied_identity = await asyncio.to_thread(httpx.delete,
                server + '/v1/device/identities/only-a?tenant=T_A',
                headers={'Authorization': 'Bearer ' + token})
            assert denied_identity.status_code == 200
            state_path = root / 'tenants/T_A/state.json'
            stale = json.loads(state_path.read_text())
            stale['agents']['only-a']['expires_at'] = 0
            state_path.write_text(json.dumps(stale))
            install_home = tmp_path / 'two-installs'
            for tenant in ('T_A', 'T_B'):
                await asyncio.to_thread(acs, env, 'mcp', 'install', 'claude', '--identity', 'me',
                                        '--tenant', tenant, '--home', str(install_home))
            entries = json.loads((install_home / '.claude.json').read_text())['mcpServers']
            assert entries['nats-hub']['env']['NATS_CREDENTIALS_FILE'] == str(a)
            assert entries['nats-hub-T_B']['env']['NATS_CREDENTIALS_FILE'] == str(b)
            d = json.loads((root / 'device.json').read_text())
            response = await asyncio.to_thread(httpx.get, server + '/v1/device?tenant=T_A',
                headers={'Authorization': 'Bearer ' + d['access_token']})
            records = response.json()['identities']
            record = next(r for r in records if r['identity'] == 'me')
            assert record['device'] == 'integration laptop'
            # The test acts as the provisioning writer; the dev stub does not
            # publish manifests itself. This stack permits its API admin to write.
            admin_env = {**env, 'NATS_CREDENTIALS_FILE': str(op / 'creds/T_A/admin-1.creds')}
            put = await asyncio.to_thread(subprocess.run, [str(REPO / 'target/debug/hub-admin'),
                'manifest', 'put', '--as', 'admin-1', '--identity', 'me', '--role', 'orchestrator',
                '--user', record['owner'], '--device', record['device']],
                env=admin_env, capture_output=True, text=True)
            assert put.returncode == 0, put.stderr
            nc = await nats.connect(os.environ['NATS_URL'], user_credentials=str(a))
            sub = await nc.subscribe('channel.task.device-name')
            await nc.flush()
            await nc.publish('hub.pub.me.task.device-name', json.dumps({
                'meta': {'id': 'device-name', 'from': 'me', 'to': None,
                         'channel': 'task.device-name', 'kind': 'message', 'reply_to': None,
                         'timestamp': '2026-11-04T00:00:00Z'}, 'payload': {}}).encode())
            msg = json.loads((await sub.next_msg(timeout=5)).data)
            assert msg['meta']['origin']['device'] == 'integration laptop'
            await nc.drain()
            print('PASS logged-in device name → provisioning record → origin.device', flush=True)
            await asyncio.to_thread(acs, env, 'agent', 'add', 'device-echo', '--role', 'worker')
            await asyncio.to_thread(acs, env, 'agent', 'add', 'device-claude', '--role', 'worker')
            workers = [spawn('echo', [str(ACS), 'worker', 'run', 'echo', '--identity', 'device-echo'], cwd=away),
                       spawn('claude', [str(ACS), 'worker', 'run', 'claude', '--identity', 'device-claude', '--',
                                        '--claude-bin', str(REPO / 'tests/python/fixtures/bin/claude'),
                                        '--repo', str(away)], cwd=away)]
            # The first run materializes the runtime and pip-installs its venv.
            for name, proc in (('echo', workers[0]), ('claude', workers[1])):
                for _ in range(1800):
                    assert proc.poll() is None, (tmp_path / f'{name}.log').read_text()
                    if '] ready' in (tmp_path / f'{name}.log').read_text():
                        break
                    await asyncio.sleep(.1)
                else:
                    raise TimeoutError(f'{name} worker startup: ' + (tmp_path / f'{name}.log').read_text())
            runtime = next((root / 'worker').glob('*-*/'))
            print(f'$ acs worker run echo|claude (no ACS_WORKER_DIR, no checkout): runtime {runtime.name}', flush=True)
            install = await asyncio.to_thread(acs, env, 'mcp', 'install', 'claude', '--identity', 'me', '--home', str(home))
            print('$ acs mcp install claude --identity me --home <tmp>\n' + install.stdout, end='', flush=True)
            config = json.loads((home / '.claude.json').read_text())['mcpServers']['nats-hub']
            assert set(config['env']) == {'NATS_URL', 'NATS_CREDENTIALS_FILE', 'NATS_HUB_IDENTITY', 'ACS_TENANT'}
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
                    claude = await session.call_tool('delegate_task', {'to': 'device-claude',
                        'prompt': 'hello claude', 'timeout': 20})
                    assert not claude.isError, claude
                    reply = json.loads(claude.content[0].text)['data']['reply']
                    assert reply['result'] == 'pong: hello claude', reply
                    print('PASS embedded claude worker (fake CLI) → pong: hello claude', flush=True)
                    await asyncio.to_thread(acs, env, 'agent', 'add', 'device-native', '--role', 'worker')
                    spawn('native', [str(ACS), 'worker', 'run', 'hub-worker', '--identity',
                                     'device-native', '--', '--execute', 'cat'])
                    await asyncio.sleep(1)
                    native = await session.call_tool('delegate_task', {'to': 'device-native',
                        'prompt': 'native worker', 'timeout': 20})
                    assert not native.isError, native
                    assert json.loads(native.content[0].text)['data']['reply']['result'].strip() == 'native worker'
                    print('PASS acs worker run hub-worker uses the selected tenant URL: native worker', flush=True)
                    cred = root / 'tenants/T_A/creds/me.creds'
                    original = cred.read_text()
                    old = tmp_path / 'unrefreshed.creds'
                    old.write_text(original)
                    old.chmod(0o600)
                    before = decode_jwt(original.splitlines()[1])
                    observer = spawn('rust-client', [str(REPO / 'target/debug/hub-observe'),
                        '--nats-url', os.environ['NATS_URL'], '--identity', 'me',
                        '--channel', 'task.acsprobe', '--json'],
                        {**config['env'], 'NATS_CREDENTIALS_FILE': str(root / 'creds/me.creds'), 'RUST_LOG': 'off'}, pipe=True)
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
                    assert (root / 'creds/me.creds').read_bytes() == cred.read_bytes()
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
                    state = json.loads((root / 'tenants/T_A/state.json').read_text())
                    refresh_args = {'identity': 'me', 'public_key': state['agents']['me']['public_key']}
                    token_headers = {'Authorization': 'Bearer ' + json.loads((root / 'device.json').read_text())['access_token']}
                    removed = await asyncio.to_thread(acs, env, 'agent', 'rm', 'me')
                    print('$ acs agent rm me\n' + removed.stdout, end='', flush=True)
                    denied = await asyncio.to_thread(httpx.post, server + '/v1/credentials/nats',
                                                     headers=token_headers, json=refresh_args)
                    assert denied.status_code == 403, denied.text
                    await asyncio.sleep(2)
                    assert not cred.exists() and 'me' not in json.loads((root / 'tenants/T_A/state.json').read_text())['agents']
                    assert b.exists() and b.read_bytes() != b_original
                    assert 'Refresh only-a failed' in (tmp_path / 'daemon.log').read_text()
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
