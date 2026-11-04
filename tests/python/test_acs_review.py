"""Upgrade recovery and device commands independent of tenant defaults."""
import fcntl
import json
import os
from pathlib import Path
import subprocess
import time

import pytest

from test_acs_cli import ACS, REPO, acs, device_stack, headless_login


def legacy(root):
    root.mkdir()
    value = dict(server='http://127.0.0.1:1', tenant='A', device='d',
                 access_token='x', access_expires_at=9999999999,
                 nats_url='nats://127.0.0.1:1', agents={})
    (root / 'state.json').write_text(json.dumps(value))
    return {**os.environ, 'ACS_HOME': str(root), 'ACS_NO_KEYRING': '1'}


@pytest.mark.parametrize('restore', ['missing', 'different'])
def test_partial_restore_preserves_seed(tmp_path, restore):
    root = tmp_path / 'acs'
    env = legacy(root)
    (root / 'creds').mkdir()
    source = root / 'creds/me.creds'
    source.write_text('only seed copy')
    target = root / 'tenants/A'
    target.mkdir(parents=True)
    if restore == 'different':
        (target / 'state.json').write_text('{}')
        (target / 'creds').mkdir()
        (target / 'creds/me.creds').write_text('other seed')
    before = (root / 'state.json').read_bytes()
    assert 'incomplete tenant restore' in acs(env, 'status', ok=False).stderr
    assert source.read_text() == 'only seed copy' and not source.is_symlink()
    assert (root / 'state.json').read_bytes() == before
    assert not (root / 'device.json').exists()


def test_running_old_daemon_warning_and_crash_residue(tmp_path):
    root = tmp_path / 'acs'
    env = legacy(root)
    home = tmp_path / 'home'
    home.mkdir()
    env['HOME'] = str(home)
    with (root / 'daemon.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        result = acs(env, 'status')
        assert 'WARNING' in result.stderr and 'run acs up' in result.stderr
    (root / '.migrate-123').mkdir()
    (root / '.migrate-123/seed').write_text('secret')
    (root / 'state.json').write_text('obsolete')
    acs(env, 'status')
    assert not (root / '.migrate-123').exists()
    assert not (root / 'state.json').exists()


@pytest.mark.parametrize('client', ['claude', 'codex', 'hermes'])
def test_config_install_preserves_home_and_config_mode(device_stack, client):
    env, tmp, _ = device_stack
    headless_login(env)
    acs(env, 'agent', 'add', 'me')
    home = tmp / 'external-home'
    home.mkdir(mode=0o755)
    config = home / {'claude': '.claude.json', 'codex': '.codex/config.toml',
                     'hermes': '.hermes/config.yaml'}[client]
    if client == 'claude':
        config.write_text('{}')
        config.chmod(0o640)
    acs(env, 'mcp', 'install', client, '--home', str(home))
    assert home.stat().st_mode & 0o777 == 0o755
    if client == 'claude':
        assert config.stat().st_mode & 0o777 == 0o640
    assert 'ACS_TENANT' in config.read_text()
    acs(env, 'logout')


@pytest.mark.parametrize('current', ['stale', None])
def test_device_commands_ignore_default_and_refresh_membership(device_stack, current):
    env, _, _ = device_stack
    headless_login(env)
    root = Path(env['ACS_HOME'])
    d = json.loads((root / 'device.json').read_text())
    d['tenants'].append({'tenant': 'removed'})
    d['tenant'] = 'removed'
    (root / 'device.json').write_text(json.dumps(d))
    if current:
        (root / 'current').write_text(current)
    else:
        (root / 'current').unlink()
    env = {**env, 'ACS_TENANT': 'invalid.default'}
    proc = subprocess.Popen([str(ACS), 'up', '--foreground'], env=env, cwd=REPO,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        for _ in range(100):
            if (root / 'daemon.pid').exists():
                break
            assert proc.poll() is None
            time.sleep(.02)
        assert (root / 'daemon.pid').exists()
        acs(env, 'down')
        assert proc.wait(timeout=5) == 0
    finally:
        if proc.poll() is None:
            proc.terminate()
            proc.wait(timeout=5)
    acs(env, 'tenant', 'list')
    assert not (root / 'current').exists()
    acs(env, 'tenant', 'use', 'T_A')
    (root / 'current').write_text('stale')
    acs(env, 'logout')
    acs(env, 'down')


def test_exec_nested_tenant_and_case_collision(device_stack):
    env, _, _ = device_stack
    headless_login(env)
    acs(env, 'agent', 'add', 'me')
    out = acs({**env, 'ACS_TENANT': 'other'}, 'exec', '--tenant', 'T_A', 'me', '--',
              'sh', '-c', 'printf "%s" "$ACS_TENANT"').stdout
    assert out == 'T_A'
    root = Path(env['ACS_HOME'])
    d = json.loads((root / 'device.json').read_text())
    d['tenants'].append({'tenant': 't_a'})
    (root / 'device.json').write_text(json.dumps(d))
    assert 'conflicts' in acs(env, 'env', '--tenant', 't_a', 'me', ok=False).stderr
    acs(env, 'logout')

@pytest.fixture
def response_server():
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    import threading
    state = {'members': [{'tenant': 'A', 'personal': False}, {'tenant': 'B', 'personal': False}],
             'gets': 0, 'old': False, 'invalid': False, 'register': None, 'subject': False}

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def respond(self, value, status=200):
            raw = value.encode() if isinstance(value, str) else json.dumps(value).encode()
            self.send_response(status)
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def do_GET(self):
            state['gets'] += 1
            if state['old']:
                return self.respond('not found', 404)
            self.respond({'tenants': state['members']})

        def do_DELETE(self):
            self.respond({'error': 'access_denied'}, 403)

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
            if self.path == '/device/token':
                self.respond(dict(tenant='A', device='test', access_token='access',
                                  refresh_token='rotated', expires_in=3600,
                                  nats_url='nats://127.0.0.1:1',
                                  tenants=['../bad'] if state['invalid'] else state['members']))
            elif self.path == '/v1/device/identities':
                data = json.loads(body)
                state['public_key'] = data['public_key']
                self.respond({'tenant': state['register'] or data['tenant']})
            else:
                import base64
                data = json.loads(body)
                claims = base64.urlsafe_b64encode(json.dumps({'sub': 'wrong' if state['subject'] else state['public_key']}).encode()).decode().rstrip('=')
                self.respond({'tenant': data['tenant'], 'jwt': 'header.' + claims + '.signature',
                              'expires_at': 9999999999})

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield f'http://127.0.0.1:{server.server_port}', state
    server.shutdown()
    server.server_close()
    thread.join(timeout=5)


def login_fake(tmp_path, response_server):
    url, state = response_server
    root = tmp_path / 'acs'
    env = {**os.environ, 'ACS_HOME': str(root), 'ACS_SERVER': url, 'ACS_NO_KEYRING': '1'}
    acs(env, 'login', '--key', 'test')
    return env, root, state


def test_rotation_is_stored_before_invalid_membership(tmp_path, response_server):
    env, root, state = login_fake(tmp_path, response_server)
    (root / 'refresh.token').write_text('old-refresh')
    device = json.loads((root / 'device.json').read_text())
    device['access_expires_at'] = 0
    (root / 'device.json').write_text(json.dumps(device))
    state['invalid'] = True
    assert 'invalid tenant slug' in acs(env, 'tenant', 'list', ok=False).stderr
    assert (root / 'refresh.token').read_text() == 'rotated'


def test_no_personal_default_and_named_mcp_entry(tmp_path, response_server):
    env, root, _ = login_fake(tmp_path, response_server)
    assert not (root / 'current').exists()
    assert json.loads((root / 'device.json').read_text())['device_name'] is None
    acs(env, 'agent', 'add', 'me', '--tenant', 'A')
    home = tmp_path / 'home'
    acs(env, 'mcp', 'install', 'claude', '--tenant', 'A', '--home', str(home),
        '--server', str(REPO / 'mcp_server/nats_hub_mcp.py'))
    entries = json.loads((home / '.claude.json').read_text())['mcpServers']
    assert list(entries) == ['nats-hub-A']


def test_explicit_miss_refreshes_once_and_removed_identity_is_local_only(tmp_path, response_server):
    env, root, state = login_fake(tmp_path, response_server)
    state['members'].append({'tenant': 'C'})
    acs(env, 'agent', 'add', 'me', '--tenant', 'C')
    assert state['gets'] == 1
    credential = root / 'tenants/C/creds/me.creds'
    assert credential.is_file()
    state['members'] = [{'tenant': 'A'}]
    acs(env, 'tenant', 'list')
    assert 'locally' in acs(env, 'agent', 'rm', 'me', '--tenant', 'C').stdout
    assert not credential.exists()
    assert not json.loads((root / 'tenants/C/state.json').read_text())['agents']


def test_plain_text_404_fallback_and_terminal_sanitizing(tmp_path, response_server):
    env, root, state = login_fake(tmp_path, response_server)
    state['old'] = True
    assert 'A' in acs(env, 'tenant', 'list').stdout
    state['old'] = False
    state['members'] = [{'tenant': 'A', 'role': '\x1b[31mowner\n',
                         'handle': '\rhandle', 'state': 'ready\x07'}]
    out = acs(env, 'tenant', 'list').stdout
    assert not any(c in out for c in ('\x1b', '\r', '\x07'))


@pytest.mark.parametrize('mismatch', ['register', 'subject'])
def test_registration_and_jwt_key_labels_are_checked(tmp_path, response_server, mismatch):
    env, root, state = login_fake(tmp_path, response_server)
    state[mismatch] = 'B' if mismatch == 'register' else True
    error = acs(env, 'agent', 'add', 'me', '--tenant', 'A', ok=False).stderr
    assert ('registration tenant mismatch' if mismatch == 'register' else 'credential public key mismatch') in error
    credential = root / 'tenants/A/creds/me.creds'
    assert not credential.exists() or 'signature' not in credential.read_text()


@pytest.mark.skipif(not os.sys.platform.startswith('linux'), reason='systemd user service')
def test_upgrade_restarts_installed_service_after_commit(tmp_path):
    root = tmp_path / 'acs'
    env = legacy(root)
    home = tmp_path / 'home'
    unit = home / '.config/systemd/user/acs.service'
    unit.parent.mkdir(parents=True)
    unit.write_text('[Service]')
    bindir = tmp_path / 'bin'
    bindir.mkdir()
    systemctl = bindir / 'systemctl'
    systemctl.write_text('#!/bin/sh\ntest -f "$ACS_HOME/device.json" || exit 1\nprintf "%s\\n" "$@" > "$ACS_HOME/restarted"\n')
    systemctl.chmod(0o755)
    env.update(HOME=str(home), PATH=str(bindir) + ':' + env['PATH'])
    with (root / 'daemon.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        assert 'WARNING' not in acs(env, 'status').stderr
    assert (root / 'restarted').read_text().splitlines() == ['--user', 'restart', 'acs.service']


def test_device_name_is_trimmed_after_truncation(device_stack):
    env, _, _ = device_stack
    headless_login({**env, 'ACS_DEVICE_NAME': 'x' * 63 + ' ' + 'suffix'})
    d = json.loads((Path(env['ACS_HOME']) / 'device.json').read_text())
    assert d['device_name'] == 'x' * 63
    acs(env, 'logout')


def test_removed_membership_with_stale_cache_allows_local_cleanup(tmp_path, response_server):
    env, root, state = login_fake(tmp_path, response_server)
    acs(env, 'agent', 'add', 'me', '--tenant', 'B')
    state['members'] = [{'tenant': 'A'}]
    assert 'locally' in acs(env, 'agent', 'rm', 'me', '--tenant', 'B').stdout
    assert not (root / 'tenants/B/creds/me.creds').exists()


def test_plain_text_old_server_falls_back_to_single_tenant(tmp_path, response_server):
    url, state = response_server
    # A legacy token response has no membership list. Reproduce its committed cache.
    root = tmp_path / 'acs'
    env = legacy(root)
    value = json.loads((root / 'state.json').read_text())
    value['server'] = url
    (root / 'state.json').write_text(json.dumps(value))
    state['old'] = True
    assert '*A' in acs(env, 'tenant', 'list').stdout
    acs(env, 'tenant', 'use', 'A')
    assert (root / 'current').read_text() == 'A'
