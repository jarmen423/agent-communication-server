"""DEV ONLY stub control plane: OAuth authorization server + NATS issuer.

In-process (httpx ASGI transport), no NATS needed.
"""
import asyncio
import base64
import hashlib
import json
import os
import secrets
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs, urlencode, urlparse

import httpx
import nkeys
import pytest

from stub_control_plane.__main__ import main as stub_main
from stub_control_plane.app import create_app
from stub_control_plane.config import StubConfig
from stub_control_plane.nats_issuer import TenantKeys, check_public_key, decode_jwt
from stub_control_plane.store import Store
from stub_control_plane.tokens import TokenIssuer

ISSUER = "http://127.0.0.1:9300"
RESOURCE = "http://127.0.0.1:9400/mcp"
CLAUDE = "https://claude.ai/api/mcp/auth_callback"
SERVICE_KEY = "k" * 43


def _seed(prefix):
    return nkeys.encode_seed(os.urandom(32), prefix)


def tenant_keys():
    account = nkeys.from_seed(_seed(nkeys.PREFIX_BYTE_ACCOUNT)).public_key.decode()
    return TenantKeys("T_A", account, {"orchestrator": _seed(nkeys.PREFIX_BYTE_ACCOUNT)})


def make(**cfg):
    keys = tenant_keys()
    config = StubConfig(issuer=ISSUER, resources=[RESOURCE], tenant="T_A",
                        tenants={"T_A": keys}, **cfg)
    store = Store()
    store.service_key = SERVICE_KEY
    issuer = TokenIssuer(ISSUER)
    app = create_app(config, store, issuer)
    http = httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url=ISSUER)
    return http, issuer, keys, store


def pkce():
    verifier = secrets.token_urlsafe(48)
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest())
    return verifier, challenge.rstrip(b"=").decode()


async def register(http, redirect=CLAUDE, **extra):
    return await http.post("/register", json={
        "client_name": "test", "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"], "token_endpoint_auth_method": "none", **extra})


async def authorize(http, client_id, redirect=CLAUDE, challenge=None, **params):
    query = {"response_type": "code", "client_id": client_id, "redirect_uri": redirect,
             "state": "st", "resource": RESOURCE, **params}
    if challenge is not None:
        query.update(code_challenge=challenge, code_challenge_method="S256")
    resp = await http.get("/authorize?" + urlencode(query))
    if resp.status_code != 302 or "/consent" not in resp.headers["location"]:
        return resp, None
    consent = await http.get(resp.headers["location"])
    assert consent.status_code == 302, consent.text
    loc = consent.headers["location"]
    assert loc.startswith(redirect)
    q = parse_qs(urlparse(loc).query)
    assert q["state"] == ["st"]
    return consent, q["code"][0]


async def token(http, client_id, code, verifier, redirect=CLAUDE):
    return await http.post("/token", data={
        "grant_type": "authorization_code", "code": code, "code_verifier": verifier,
        "client_id": client_id, "redirect_uri": redirect})


async def login(http, redirect=CLAUDE):
    client = (await register(http, redirect)).json()
    verifier, challenge = pkce()
    _, code = await authorize(http, client["client_id"], redirect, challenge)
    resp = await token(http, client["client_id"], code, verifier, redirect)
    assert resp.status_code == 200, resp.text
    return client, resp.json()


def run(coro):
    return asyncio.run(coro)


def test_metadata_advertises_pkce_dcr_cimd_refresh():
    async def go():
        http, issuer, _, _ = make()
        meta = (await http.get("/.well-known/oauth-authorization-server")).json()
        assert meta["issuer"].rstrip("/") == ISSUER
        assert meta["code_challenge_methods_supported"] == ["S256"]
        assert meta["registration_endpoint"] == f"{ISSUER}/register"
        assert meta["client_id_metadata_document_supported"] is True
        assert set(meta["grant_types_supported"]) == {"authorization_code", "refresh_token"}
        assert "none" in meta["token_endpoint_auth_methods_supported"]
        jwks = (await http.get("/.well-known/jwks.json")).json()
        assert jwks["keys"][0]["kid"] == issuer.kid and jwks["keys"][0]["crv"] == "Ed25519"
        assert "DEV ONLY" in (await http.get("/")).text
    run(go())


@pytest.mark.parametrize("uri,ok", [
    (CLAUDE, True),
    ("https://claude.com/api/mcp/auth_callback", True),
    ("https://chatgpt.com/connector_platform_oauth_redirect", True),
    ("https://chatgpt.com/connector/oauth/abc_123-XYZ", True),
    ("https://chatgpt.com/connector/oauth/a/../../evil", False),
    ("https://claude.ai.evil.example/api/mcp/auth_callback", False),
    ("https://evil.example/callback", False),
    ("http://127.0.0.1:8080/callback", False),  # loopback only when enabled
])
def test_dcr_redirect_allowlist(uri, ok):
    async def go():
        http, *_ = make()
        resp = await register(http, uri)
        assert (resp.status_code == 201) is ok, resp.text
        if not ok:
            assert resp.json()["error"] == "invalid_redirect_uri"
    run(go())


def test_loopback_redirects_when_enabled():
    async def go():
        http, *_ = make(allow_loopback_redirects=True)
        assert (await register(http, "http://127.0.0.1:8080/callback")).status_code == 201
        assert (await register(http, "http://127.0.0.2.evil:80/cb")).status_code == 400
    run(go())


def test_pkce_s256_required_and_codes_single_use():
    async def go():
        http, issuer, _, _ = make()
        client = (await register(http)).json()
        cid = client["client_id"]
        # No challenge, or the plain method: refused.
        resp, code = await authorize(http, cid)
        assert code is None and "error" in (resp.headers.get("location", "") + resp.text)
        resp, code = await authorize(http, cid, code_challenge="x" * 43,
                                     code_challenge_method="plain")
        assert code is None and "error" in (resp.headers.get("location", "") + resp.text)
        verifier, challenge = pkce()
        _, code = await authorize(http, cid, challenge=challenge)
        bad = await token(http, cid, code, pkce()[0])
        assert bad.status_code == 400 and bad.json()["error"] == "invalid_grant"
        good = await token(http, cid, code, verifier)
        assert good.status_code == 200, good.text
        again = await token(http, cid, code, verifier)
        assert again.status_code == 400 and again.json()["error"] == "invalid_grant"
        claims = issuer.verify(good.json()["access_token"], audience=RESOURCE)
        assert claims["acs_tenant"] == "T_A" and claims["acs_identity"] == "claude-web-1"
        assert claims["scope"] == "acs:hub" and claims["client_id"] == cid
        # A different resource is refused.
        resp, code = await authorize(http, cid, challenge=challenge,
                                     resource="https://other.example/mcp")
        assert code is None
    run(go())


def test_identity_per_connector_install():
    async def go():
        http, issuer, _, _ = make()
        client, tok = await login(http)
        first = issuer.verify(tok["access_token"])["acs_identity"]
        verifier, challenge = pkce()
        _, code = await authorize(http, client["client_id"], challenge=challenge)
        again = (await token(http, client["client_id"], code, verifier)).json()
        assert issuer.verify(again["access_token"])["acs_identity"] == first
        _, other = await login(http, "https://chatgpt.com/connector/oauth/cb1")
        assert issuer.verify(other["access_token"])["acs_identity"] == "chatgpt-web-1"
        _, third = await login(http)
        assert issuer.verify(third["access_token"])["acs_identity"] == "claude-web-2"
    run(go())


def test_refresh_rotation_and_reuse_revokes_family():
    async def go():
        http, issuer, _, _ = make()
        client, tok = await login(http)
        form = {"grant_type": "refresh_token", "client_id": client["client_id"],
                "refresh_token": tok["refresh_token"]}
        new = (await http.post("/token", data=form)).json()
        assert new["refresh_token"] != tok["refresh_token"]
        assert issuer.verify(new["access_token"], audience=RESOURCE)
        assert (await http.post("/token", data=form)).status_code == 400  # reuse
        form["refresh_token"] = new["refresh_token"]
        assert (await http.post("/token", data=form)).status_code == 400  # family revoked
    run(go())


def test_nats_credentials_api():
    async def go():
        http, issuer, keys, store = make()
        _, tok = await login(http)
        user = nkeys.from_seed(_seed(nkeys.PREFIX_BYTE_USER))
        pub = user.public_key.decode()
        auth = {"Authorization": f"Bearer {tok['access_token']}",
                "X-ACS-Service-Key": SERVICE_KEY}
        url = "/v1/credentials/nats"
        assert (await http.post(url, json={"public_key": pub},
                                headers={"Authorization": auth["Authorization"]})).status_code == 401
        assert (await http.post(url, json={"public_key": pub},
                                headers={"X-ACS-Service-Key": SERVICE_KEY})).status_code == 401
        forged = tok["access_token"][:-4] + "AAAA"
        assert (await http.post(url, json={"public_key": pub},
                                headers={**auth, "Authorization": f"Bearer {forged}"})).status_code == 401
        bad = await http.post(url, json={"public_key": "UABC"}, headers=auth)
        assert bad.status_code == 400
        resp = await http.post(url, json={"public_key": pub}, headers=auth)
        assert resp.status_code == 200, resp.text
        body = resp.json()
        claims = decode_jwt(body["jwt"])
        signer = nkeys.from_seed(keys.signing_seeds["orchestrator"])
        assert claims["iss"] == signer.public_key.decode()
        assert claims["sub"] == pub and claims["name"] == body["identity"] == "claude-web-1"
        assert claims["nats"] == {"pub": {}, "sub": {}, "issuer_account": keys.account_public_key,
                                  "type": "user", "version": 2}
        assert claims["exp"] == body["expires_at"]
        head, payload, sig = body["jwt"].split(".")
        assert signer.verify(f"{head}.{payload}".encode(),
                             base64.urlsafe_b64decode(sig + "=" * (-len(sig) % 4)))
        assert store.identity("T_A", "claude-web-1").nats_public_keys == [pub]
        # Revoking the grant family (refresh-token reuse, /revoke) stops a
        # still-valid access token from minting NATS creds; the family is
        # persisted, so this also holds after a stub restart.
        grant = decode_jwt(tok["access_token"])["acs_grant"]
        store.revoke_family(grant)
        denied = await http.post(url, json={"public_key": pub}, headers=auth)
        assert denied.status_code == 401 and "grant" in denied.text
        store.grants[grant].revoked = False
        assert (await http.post(url, json={"public_key": pub}, headers=auth)).status_code == 200
        # Revoking the identity stops issuance at once (the grant check sees
        # it first, so 401; the identity check is the 403 backstop).
        store.identity("T_A", "claude-web-1").revoked = True
        assert (await http.post(url, json={"public_key": pub}, headers=auth)).status_code in (401, 403)
    run(go())


def test_public_key_checks():
    pub = nkeys.from_seed(_seed(nkeys.PREFIX_BYTE_USER)).public_key.decode()
    assert check_public_key(pub) == pub
    acct = nkeys.from_seed(_seed(nkeys.PREFIX_BYTE_ACCOUNT)).public_key.decode()
    for bad in (acct, pub[:-1] + ("A" if pub[-1] != "A" else "B"), "U" * 56, "", None):
        with pytest.raises(ValueError):
            check_public_key(bad)


class _Doc(BaseHTTPRequestHandler):
    doc: dict = {}

    def do_GET(self):  # noqa: N802
        body = json.dumps(self.doc).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def test_client_id_metadata_document():
    server = HTTPServer(("127.0.0.1", 0), _Doc)
    url = f"http://127.0.0.1:{server.server_port}/client.json"
    threading.Thread(target=server.serve_forever, daemon=True).start()

    async def go():
        http, issuer, _, _ = make(allow_http_cimd=True)
        _Doc.doc = {"client_id": url, "client_name": "cimd", "redirect_uris": [CLAUDE],
                    "grant_types": ["authorization_code", "refresh_token"],
                    "response_types": ["code"], "token_endpoint_auth_method": "none"}
        verifier, challenge = pkce()
        _, code = await authorize(http, url, challenge=challenge)
        resp = await token(http, url, code, verifier)
        assert resp.status_code == 200, resp.text
        assert issuer.verify(resp.json()["access_token"])["client_id"] == url
        # client_id must equal the document URL; redirects must be allowlisted.
        http2, *_ = make(allow_http_cimd=True)
        _Doc.doc = {**_Doc.doc, "client_id": "https://elsewhere.example/c.json"}
        assert (await authorize(http2, url, challenge=challenge))[1] is None
        http3, *_ = make(allow_http_cimd=True)
        _Doc.doc = {**_Doc.doc, "client_id": url, "redirect_uris": ["https://evil.example/cb"]}
        assert (await authorize(http3, url, "https://evil.example/cb", challenge))[1] is None

    try:
        run(go())
    finally:
        server.shutdown()


def test_refuses_non_loopback_bind(tmp_path):
    with pytest.raises(SystemExit) as e:
        stub_main(["--host", "0.0.0.0", "--resource", RESOURCE,
                   "--service-key-file", str(tmp_path / "k")])
    assert "DEV ONLY" in str(e.value)


def test_cimd_ssrf_guard_rejects_private_hosts():
    """Unauthenticated /authorize?client_id=<url> makes the stub fetch that
    URL; it must never reach loopback, private or link-local addresses."""
    from stub_control_plane.provider import _public_host

    async def go():
        for url in ("https://127.0.0.1/c.json", "https://localhost/c.json",
                    "https://10.0.0.5/c.json", "https://192.168.1.1/c.json",
                    "https://169.254.169.254/latest/meta-data", "https://[::1]/c.json",
                    "https://no-such-host.invalid/c.json"):
            assert not await _public_host(url), url
        assert await _public_host("https://8.8.8.8/c.json")
    run(go())
