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
