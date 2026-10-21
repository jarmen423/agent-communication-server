"""CLI against a real loopback DEV ONLY control plane (no NATS required)."""
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import time

import httpx
import pytest

from stub_control_plane.nats_issuer import decode_jwt
from test_stub_cp import tenant_keys

REPO = Path(__file__).resolve().parents[2]
ACS = REPO / 'target/debug/acs'
pytestmark = pytest.mark.skipif(not ACS.exists(), reason='build acs with make build first')


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


@pytest.fixture
def device_stack(tmp_path):
    keys = tenant_keys()
    keyfile = tmp_path / 'tenant.json'
    keyfile.write_text(json.dumps({'tenant': keys.tenant, 'account_public_key': keys.account_public_key,
                                  'signing_keys': {**{k: v.decode() for k, v in keys.signing_seeds.items()},
                                                   'worker': keys.signing_seeds['orchestrator'].decode()}}))
    keyfile.chmod(0o600)
    port = free_port()
    url = f'http://127.0.0.1:{port}'
    env = {**os.environ, 'ACS_HOME': str(tmp_path / 'acs'), 'ACS_NO_KEYRING': '1',
           'ACS_SERVER': url}
    logfile = (tmp_path / 'cp.log').open('w')
    proc = subprocess.Popen([sys.executable, '-m', 'stub_control_plane', '--port', str(port),
                             '--resource', url + '/mcp', '--tenant-key-file', str(keyfile),
                             '--service-key-file', str(tmp_path / 'admin.key'), '--access-ttl', '60'],
                            cwd=REPO, stdout=logfile, stderr=subprocess.STDOUT)
    try:
        for _ in range(100):
            assert proc.poll() is None
            try:
                if httpx.get(url + '/healthz').status_code == 200:
                    break
            except httpx.HTTPError:
                pass
            time.sleep(.05)
        else:
            raise TimeoutError('stub control plane startup')
        yield env, tmp_path, url
    finally:
        proc.terminate()
        proc.wait(timeout=10)
        logfile.close()


def acs(env, *args, ok=True):
    proc = subprocess.run([str(ACS), *args], env=env, cwd=REPO, text=True, capture_output=True, timeout=30)
    if ok:
        assert proc.returncode == 0, proc.stdout + proc.stderr
    else:
        assert proc.returncode != 0
    return proc


def headless_login(env):
    proc = subprocess.Popen([str(ACS), 'login', '--headless'], env=env, cwd=REPO,
                            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        line = proc.stdout.readline()
        url = re.search(r'https?://\S+', line).group()
        assert httpx.get(url).status_code == 200
        stdout, stderr = proc.communicate(timeout=15)
        assert proc.returncode == 0, stderr
        return line + stdout
    finally:
        if proc.poll() is None:
            proc.terminate()
            proc.wait(timeout=5)


def test_headless_login_agents_install_preserves_config_and_logout(device_stack):
    env, tmp, _ = device_stack
    assert 'Logged in: tenant T_A device device-' in headless_login(env)
    acs(env, 'agent', 'add', 'me')
    acs(env, 'agent', 'add', 'bot', '--role', 'worker')
    root = Path(env['ACS_HOME'])
    state = json.loads((root / 'state.json').read_text())
    creds = root / 'creds/me.creds'
    original = creds.read_text()
    claims = decode_jwt(original.splitlines()[1])
    assert claims['name'] == 'me' and claims['sub'] == state['agents']['me']['public_key']
    for path in (creds, root / 'refresh.token', root / 'state.json'):
        assert path.stat().st_mode & 0o777 == 0o600
    assert original.split('BEGIN USER NKEY SEED-----\n')[1].splitlines()[0] not in json.dumps(state)
    status = acs(env, 'status').stdout
    assert all(s in status for s in ('Server:', 'Tenant: T_A', 'Device:', 'Daemon: stopped', 'me:', 'bot:'))
    assert state['access_token'] not in status
    home = tmp / 'home'
    home.mkdir()
    (home / '.claude.json').write_text(json.dumps({'unrelated': 42, 'mcpServers': {'other': {'command': 'other'}}}))
    (home / '.codex').mkdir()
    (home / '.codex/config.toml').write_text('# keep this comment\nmodel = "existing"\n[mcp_servers.other]\ncommand = "other"\n')
    (home / '.hermes').mkdir()
    (home / '.hermes/config.yaml').write_text('model: existing\nmcp_servers:\n  other:\n    command: other\n')
    for client in ('claude', 'codex', 'hermes'):
        acs(env, 'mcp', 'install', client, '--identity', 'me', '--home', str(home))
        acs(env, 'mcp', 'install', client, '--identity', 'me', '--home', str(home))
    claude = json.loads((home / '.claude.json').read_text())
    assert claude['mcpServers']['nats-hub']['command'].endswith('/.venv/bin/python')
    assert claude['unrelated'] == 42 and claude['mcpServers']['other']['command'] == 'other'
    assert claude['mcpServers']['nats-hub']['env'] == {
        'NATS_URL': state['nats_url'], 'NATS_HUB_IDENTITY': 'me', 'NATS_CREDENTIALS_FILE': str(creds)}
    import tomllib
    codex = tomllib.loads((home / '.codex/config.toml').read_text())
    assert codex['model'] == 'existing' and codex['mcp_servers']['other']['command'] == 'other'
    assert '# keep this comment' in (home / '.codex/config.toml').read_text()
    # yaml parser is a Rust dependency, not a Python runtime requirement.
    assert 'model: existing' in (home / '.hermes/config.yaml').read_text()
    assert 'other:' in (home / '.hermes/config.yaml').read_text()
    before = (home / '.claude.json').read_bytes()
    acs(env, 'mcp', 'install', 'claude', '--home', str(home), ok=False)  # ambiguous identity
    assert (home / '.claude.json').read_bytes() == before
    acs(env, 'agent', 'rm', 'bot')
    assert not (root / 'creds/bot.creds').exists()
    acs(env, 'logout')
    assert not (root / 'state.json').exists() and not creds.exists()


def test_service_key_login_rotation_and_reserved_identity(device_stack):
    env, tmp, url = device_stack
    key = httpx.post(url + '/device/service-key', headers={
        'x-acs-service-key': (tmp / 'admin.key').read_text().strip()}).json()['key']
    env = {**env, 'ACS_SERVICE_KEY': key}
    assert key not in acs(env, 'login').stdout
    acs(env, 'agent', 'add', 'hub-server', ok=False)
    acs(env, 'agent', 'add', '../bad', ok=False)
    root = Path(env['ACS_HOME'])
    token = (root / 'refresh.token').read_text()
    state = json.loads((root / 'state.json').read_text())
    state['access_expires_at'] = 0
    (root / 'state.json').write_text(json.dumps(state))
    acs(env, 'agent', 'add', 'bot')
    assert (root / 'refresh.token').read_text() != token
    acs(env, 'logout')
    assert httpx.post(url + '/device/token', json={'grant_type': 'service_key', 'key': key}).status_code == 400


def test_linux_user_service_installation(device_stack):
    if sys.platform != 'linux':
        pytest.skip('systemd user service generation is Linux-specific')
    env, tmp, _ = device_stack
    headless_login(env)
    fakebin = tmp / 'bin'
    fakebin.mkdir()
    home = tmp / 'service-home'
    home.mkdir()
    systemctl = fakebin / 'systemctl'
    systemctl.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$ACS_HOME/systemctl.calls"\n')
    systemctl.chmod(0o700)
    acs({**env, 'HOME': str(home), 'PATH': str(fakebin) + os.pathsep + env['PATH']}, 'up')
    unit = (home / '.config/systemd/user/acs.service').read_text()
    assert ' up --foreground' in unit and f'ACS_HOME={env["ACS_HOME"]}' in unit
    assert str(ACS) in unit and 'Restart=on-failure' in unit
    calls = (Path(env['ACS_HOME']) / 'systemctl.calls').read_text()
    assert '--user daemon-reload' in calls and '--user enable --now acs.service' in calls
    acs(env, 'logout')


def test_keychain_unavailable_falls_back_and_logs_out(device_stack):
    env, tmp, _ = device_stack
    env = {k: v for k, v in env.items() if k not in ('ACS_NO_KEYRING', 'DISPLAY')}
    env['DBUS_SESSION_BUS_ADDRESS'] = 'unix:path=' + str(tmp / 'no-session-bus')
    headless_login(env)
    assert (Path(env['ACS_HOME']) / 'refresh.token').stat().st_mode & 0o777 == 0o600
    acs(env, 'logout')
    assert not (Path(env['ACS_HOME']) / 'refresh.token').exists()
