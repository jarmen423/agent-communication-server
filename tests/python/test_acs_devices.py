"""DEV ONLY RFC 8628 device flow, service keys and ownership boundaries."""
import asyncio
import time

import pytest

from stub_control_plane.devices import DEVICE_GRANT
from stub_control_plane.nats_issuer import decode_jwt, new_user_seed
from stub_control_plane.store import Store
from test_stub_cp import ADMIN_KEY, ISSUER, RESOURCE, SERVICE_KEY, make
import nkeys


async def login(http):
    code = (await http.post('/device/code', data={'client_id': 'acs'})).json()
    assert (await http.post('/device/verify', data={'user_code': code['user_code']})).status_code == 200
    result = await http.post('/device/token', data={
        'grant_type': DEVICE_GRANT, 'device_code': code['device_code'], 'client_id': 'acs'})
    assert result.status_code == 200, result.text
    return result.json()


def headers(tokens):
    return {'Authorization': 'Bearer ' + tokens['access_token']}


def public():
    return nkeys.from_seed(new_user_seed()).public_key.decode()


def test_device_protocol_pending_slow_down_expiry_and_single_use():
    async def go():
        http, issuer, _, store = make()
        async with http:
            code = (await http.post('/device/code', data={'client_id': 'acs'})).json()
            assert all(code[k] for k in ('device_code', 'user_code', 'verification_uri', 'expires_in', 'interval'))
            form = {'grant_type': DEVICE_GRANT, 'device_code': code['device_code'], 'client_id': 'acs'}
            assert (await http.post('/device/token', data=form)).json()['error'] == 'authorization_pending'
            assert (await http.post('/device/token', data=form)).json()['error'] == 'slow_down'
            assert 'DEV ONLY' in (await http.get(code['verification_uri_complete'])).text
            # Only advance the server's poll deadline; no slow sleeps in unit tests.
            from stub_control_plane.tokens import token_hash
            endpoint = http._transport.app.state.devices
            endpoint.codes[token_hash(code['device_code'])]['next_poll'] = 0
            t = (await http.post('/device/token', data=form)).json()
            claims = issuer.verify(t['access_token'], audience=ISSUER)
            assert claims['acs_tenant'] == 'T_A' and claims['scope'] == 'acs:device'
            assert issuer.verify(t['access_token'], audience=RESOURCE) is None
            assert (await http.post('/device/token', data=form)).json()['error'] == 'expired_token'
            code = (await http.post('/device/code', data={'client_id': 'acs'})).json()
            endpoint.codes[token_hash(code['device_code'])]['expires'] = time.time() - 1
            assert (await http.post('/device/token', data={**form, 'device_code': code['device_code']})).status_code == 400
            assert (await http.get(code['verification_uri_complete'])).status_code == 400
            assert (await http.post('/device/token', json={'grant_type': 'wrong'})).status_code == 400
            assert (await http.post('/device/verify', json=[])).status_code == 400
    asyncio.run(go())


def test_device_identity_ownership_tenants_revocation_and_refresh_replay(tmp_path):
    async def go():
        http, _, _, store = make()
        store.path = tmp_path / 'state.json'
        async with http:
            a, b = await login(http), await login(http)
            ha, hb = headers(a), headers(b)
            pk = public()
            args = {'identity': 'me', 'public_key': pk}
            assert (await http.post('/v1/device/identities', json=args)).status_code == 401
            assert (await http.post('/v1/device/identities', json=args, headers=ha)).status_code == 201
            assert (await http.post('/v1/device/identities', json=args, headers=hb)).status_code == 409
            mint = {'identity': 'me', 'public_key': pk}
            good = await http.post('/v1/credentials/nats', json=mint, headers=ha)
            assert good.status_code == 200, good.text
            assert decode_jwt(good.json()['jwt'])['name'] == 'me'
            for data in (mint, {**mint, 'tenant': 'T_B'}, {**mint, 'public_key': public()}):
                assert (await http.post('/v1/credentials/nats', json=data, headers=hb)).status_code == 403
            assert (await http.post('/v1/credentials/nats', json={**mint, 'tenant': 'T_B'}, headers=ha)).status_code == 403
            assert (await http.delete('/v1/device/identities/me', headers=hb)).status_code == 403
            assert (await http.get('/v1/device', headers=hb)).json()['identities'] == []
            assert (await http.delete('/v1/device/identities/me', headers=ha)).status_code == 200
            assert (await http.post('/v1/credentials/nats', json=mint, headers=ha)).status_code == 403
            form = {'grant_type': 'refresh_token', 'refresh_token': a['refresh_token']}
            rotated = (await http.post('/device/token', data=form)).json()
            assert rotated['refresh_token'] != a['refresh_token']
            assert (await http.post('/device/token', data=form)).status_code == 400
            assert (await http.get('/v1/device', headers=headers(rotated))).status_code == 401
            assert (await http.delete('/v1/device', headers=hb)).status_code == 200
            assert (await http.get('/v1/device', headers=hb)).status_code == 401
            assert (await http.post('/device/token', data={'grant_type': 'refresh_token', 'refresh_token': b['refresh_token']})).status_code == 400
            persisted = Store(store.path)
            assert persisted.devices[b['device']]['revoked']
            assert a['refresh_token'] not in store.path.read_text()
            assert store.path.stat().st_mode & 0o777 == 0o600
    asyncio.run(go())


@pytest.mark.parametrize('extra', [
    {'identity': 'hub-server'}, {'identity': 'a.b'}, {'identity': 'wave'},
    {'identity': ''}, {'role': 'admin'}, {'public_key': 'SU-not-a-public-key'},
    {'tenant': 'T_B'},
])
def test_registration_validation(extra):
    async def go():
        http, *_ = make()
        async with http:
            t = await login(http)
            r = await http.post('/v1/device/identities', headers=headers(t),
                                json={'identity': 'me', 'public_key': public(), **extra})
            assert r.status_code in (400, 403), r.text
    asyncio.run(go())


def test_service_keys_and_device_revocation():
    async def go():
        http, *_ = make()
        async with http:
            assert (await http.post('/device/service-key')).status_code == 401
            # The MCP endpoint's service key is not an admin credential.
            assert (await http.post('/device/service-key',
                                    headers={'x-acs-service-key': SERVICE_KEY})).status_code == 401
            assert (await http.post('/device/service-key',
                                    headers={'x-acs-admin-key': SERVICE_KEY})).status_code == 401
            admin = {'x-acs-admin-key': ADMIN_KEY}
            key = (await http.post('/device/service-key', headers=admin)).json()
            form = {'grant_type': 'service_key', 'key': key['key']}
            t = (await http.post('/device/token', data=form)).json()
            assert t['device'] == key['device']
            assert (await http.post('/v1/device/identities', headers=headers(t),
                                    json={'identity': 'bot', 'role': 'worker', 'public_key': public()})).status_code == 201
            assert (await http.request('DELETE', '/device/service-key', headers=admin,
                                       json={'device': key['device']})).status_code == 200
            assert (await http.post('/device/token', data=form)).status_code == 400
            assert (await http.get('/v1/device', headers=headers(t))).status_code == 401
            assert (await http.post('/v1/credentials/nats', headers=headers(t),
                                    json={'identity': 'bot', 'public_key': public()})).status_code == 401
    asyncio.run(go())


def test_jwt_inbox_hint_and_explicit_override(tmp_path, monkeypatch):
    import base64
    import json
    from nats_connect import credentials_identity, inbox_prefix_for
    monkeypatch.delenv('NATS_HUB_IDENTITY', raising=False)
    path = tmp_path / 'identity.creds'
    payload = base64.urlsafe_b64encode(json.dumps({'name': 'hub-server'}).encode()).rstrip(b'=').decode()
    path.write_text(f'-----BEGIN NATS USER JWT-----\nx.{payload}.y\n')
    assert credentials_identity(str(path)) == 'hub-server'
    assert inbox_prefix_for(credentials_file=str(path), name='logical-client') == '_INBOX.hub-server'
    # The credential name is the permission key and wins (same as Rust).
    assert inbox_prefix_for(credentials_file=str(path), identity='explicit') == '_INBOX.hub-server'
    assert inbox_prefix_for(identity='explicit') == '_INBOX.explicit'
    path.write_text('BEGIN NATS USER JWT\nx.bad!.y\n')
    assert credentials_identity(str(path)) is None


def test_device_public_client_identifier_is_required():
    async def go():
        http, *_ = make()
        async with http:
            for client in ({}, {'client_id': ''}, {'client_id': 'foreign-client'}):
                response = await http.post('/device/code', data=client)
                assert response.status_code == 400
                assert response.json()['error'] == 'invalid_client'
            code = (await http.post('/device/code', data={'client_id': 'acs'})).json()
            assert (await http.get(code['verification_uri_complete'])).status_code == 200
            for client in ({}, {'client_id': 'foreign-client'}):
                response = await http.post('/device/token', data={
                    'grant_type': DEVICE_GRANT, 'device_code': code['device_code'], **client})
                assert response.status_code == 400
                assert response.json()['error'] == 'invalid_client'
            response = await http.post('/device/token', data={
                'grant_type': DEVICE_GRANT, 'device_code': code['device_code'], 'client_id': 'acs'})
            assert response.status_code == 200
    asyncio.run(go())
