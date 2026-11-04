"""Tenant separation, migration, and device naming through the real CLI."""
import json
import os
from pathlib import Path
import shutil
import subprocess

import httpx
import pytest

from test_acs_cli import ACS, REPO, acs, device_stack, headless_login


def state(root, tenant='T_A'):
    return json.loads((root / 'tenants' / tenant / 'state.json').read_text())


def test_device_name_and_tenant_api(device_stack):
    env, _, url = device_stack
    headless_login({**env, 'ACS_DEVICE_NAME': '  test laptop  '})
    root = Path(env['ACS_HOME'])
    device = json.loads((root / 'device.json').read_text())
    headers = {'authorization': 'Bearer ' + device['access_token']}
    acs(env, 'agent', 'add', 'me', '--tenant', 'T_A')
    records = httpx.get(url + '/v1/device?tenant=*', headers=headers).json()
    assert records['tenants'][0]['identities'][0]['device'] == 'test laptop'
    assert '*T_A' in acs(env, 'tenant', 'list').stdout
    acs(env, 'tenant', 'use', 'T_A')
    assert httpx.get(url + '/v1/tenants/T_A/members', headers=headers).json()['members']
    for slug in ('../bad', 'a.b', 'x' * 65, ''):
        acs(env, 'env', '--tenant', slug, 'me', ok=False)
    assert httpx.post(url + '/v1/device/identities', headers=headers,
                      json={'identity': 'foreign', 'tenant': '../bad', 'public_key': 'bad'}).status_code in (400, 403)
    for path in [root, root / 'tenants', root / 'tenants/T_A', root / 'tenants/T_A/creds']:
        assert path.stat().st_mode & 0o777 == 0o700
    assert (root / 'tenants/T_A/creds/me.creds').stat().st_mode & 0o777 == 0o600


def test_migration_preserves_old_credential_path_and_keyring_location(device_stack):
    env, _, _ = device_stack
    headless_login(env)
    acs(env, 'agent', 'add', 'me')
    root = Path(env['ACS_HOME'])
    d = json.loads((root / 'device.json').read_text())
    old = {**d, **state(root)}
    old.pop('tenants')
    credential = (root / 'tenants/T_A/creds/me.creds').read_bytes()
    shutil.copytree(root / 'tenants/T_A/creds', root / 'creds')
    shutil.rmtree(root / 'tenants')
    (root / 'device.json').unlink()
    (root / 'current').unlink()
    (root / 'state.json').write_text(json.dumps(old))
    refresh = (root / 'refresh.token').read_bytes()
    abandoned = root / '.migrate-999999'
    abandoned.mkdir()
    (abandoned / 'device.json').write_text('interrupted')
    acs(env, 'status')
    assert not abandoned.exists()
    assert json.loads((root / 'state.json.bak').read_text()) == old
    assert (root / 'creds/me.creds').is_symlink()
    assert (root / 'creds/me.creds').read_bytes() == credential
    assert (root / 'refresh.token').read_bytes() == refresh
    before = (root / 'device.json').read_bytes()
    acs(env, 'status')
    assert (root / 'device.json').read_bytes() == before
    acs(env, 'agent', 'rm', 'me')
    assert not (root / 'creds/me.creds').exists()


def test_old_server_membership_fallback_and_ambiguous_selection(tmp_path):
    root = tmp_path / 'acs'
    root.mkdir()
    (root / 'state.json').write_text(json.dumps({
        'server': 'http://127.0.0.1:1', 'tenant': 'T_A', 'device': 'legacy',
        'nats_url': 'nats://127.0.0.1:1', 'access_token': 'unused',
        'access_expires_at': 9999999999, 'agents': {}}))
    env = {**os.environ, 'ACS_HOME': str(root), 'ACS_NO_KEYRING': '1'}
    acs(env, 'status')
    d = json.loads((root / 'device.json').read_text())
    d['tenants'].append({'tenant': 'T_B'})
    (root / 'device.json').write_text(json.dumps(d))
    (root / 'current').unlink()
    error = acs(env, 'env', 'me', ok=False).stderr
    assert 'T_A, T_B' in error
    error = acs(env, 'exec', '--tenant', 'T_B', 'me', '--', 'true', ok=False).stderr
    assert 'identity is not configured' in error


def test_device_hostname_is_sent_by_default(device_stack):
    import socket
    env, _, url = device_stack
    env = {k: v for k, v in env.items() if k != 'ACS_DEVICE_NAME'}
    headless_login(env)
    acs(env, 'agent', 'add', 'host')
    device = json.loads((Path(env['ACS_HOME']) / 'device.json').read_text())
    result = httpx.get(url + '/v1/device?tenant=T_A',
                      headers={'Authorization': 'Bearer ' + device['access_token']}).json()
    assert result['tenants'][0]['identities'][0]['device'] == socket.gethostname().strip()[:64]


def test_server_tenant_slugs_are_validated_before_paths_are_written(tmp_path):
    root = tmp_path / 'acs'
    root.mkdir()
    (root / 'state.json').write_text(json.dumps({
        'server': 'http://127.0.0.1:1', 'tenant': '../escaped', 'device': 'legacy',
        'nats_url': 'nats://127.0.0.1:1', 'access_token': 'unused',
        'access_expires_at': 9999999999, 'agents': {}}))
    env = {**os.environ, 'ACS_HOME': str(root)}
    assert 'invalid tenant slug' in acs(env, 'status', ok=False).stderr
    assert not (tmp_path / 'escaped').exists()
    assert not (root / 'device.json').exists()


@pytest.mark.parametrize('legacy', [False, True])
@pytest.mark.parametrize('gated', [False, True])
def test_credential_response_cannot_cross_tenant_directories(tmp_path, legacy, gated):
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    import threading

    requests = []
    selected = 'A' if legacy else 'B'
    foreign = 'B' if legacy else 'A'

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_POST(self):
            raw = self.rfile.read(int(self.headers.get('Content-Length', 0)))
            requests.append((self.path, raw))
            status = 200
            if self.path == '/device/token':
                result = {'tenant': 'A', 'tenants': ['A', 'B'], 'device': 'test-device',
                          'access_token': 'test-access', 'refresh_token': 'test-refresh',
                          'expires_in': 3600, 'nats_url': 'nats://127.0.0.1:1'}
                if legacy:
                    result.pop('tenants')
            elif self.path == '/v1/device/identities':
                result = {'tenant': selected, 'identity': 'me', 'role': 'orchestrator'}
                if gated:
                    status = 403
                    result = {'error': 'access_denied', 'error_description': 'untrusted server details'}
            else:
                result = {'tenant': foreign, 'jwt': 'foreign-credential', 'expires_at': 9999999999}
            payload = json.dumps(result).encode()
            self.send_response(status)
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    root = tmp_path / 'acs'
    env = {**os.environ, 'ACS_HOME': str(root), 'ACS_NO_KEYRING': '1',
           'ACS_SERVER': f'http://127.0.0.1:{server.server_port}'}
    try:
        acs(env, 'login', '--key', 'test-key')
        error = acs(env, 'agent', 'add', 'me', '--tenant', selected, ok=False).stderr
        if gated:
            assert 'cannot add an identity in this tenant' in error
            assert 'untrusted server details' not in error
            assert not (root / 'tenants' / selected / 'creds/me.creds').exists()
        else:
            assert 'credential tenant mismatch' in error
            assert 'foreign-credential' not in (root / 'tenants' / selected / 'creds/me.creds').read_text()
        assert not (root / 'tenants' / foreign / 'creds/me.creds').exists()
        for path, raw in requests:
            if path.startswith('/v1/'):
                assert json.loads(raw)['tenant'] == selected
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
