"""HTTP API of the DEV ONLY stub control plane.

OAuth authorization server for the remote MCP endpoint (``mcp_remote/``):

| Route | What |
|---|---|
| ``GET /.well-known/oauth-authorization-server`` | RFC 8414 metadata (DCR, CIMD, PKCE S256, refresh) |
| ``GET /.well-known/jwks.json`` | access-token signing key (EdDSA) |
| ``POST /register`` | RFC 7591 dynamic client registration (redirect allowlist) |
| ``GET /authorize`` | authorization-code request (PKCE S256 required) → ``/consent`` |
| ``GET /consent`` | auto-approves the fixed dev user; mints the connector identity |
| ``POST /token`` | code exchange; refresh with rotation |
| ``POST /revoke`` | RFC 7009 revocation |
| ``POST /v1/credentials/nats`` | NATS user JWT for the token's identity and the caller's public nkey |
| ``GET /healthz`` | liveness |

Device routes (DEV ONLY): ``POST /device/code``, ``POST /device/token``,
``GET|POST /device/verify`` (auto-approve), ``POST|DELETE /device/service-key``
(admin bootstrap), ``GET|DELETE /v1/device``, and
``POST /v1/device/identities`` / ``DELETE /v1/device/identities/{identity}``.
Device access tokens target this control plane and mint only their own
registered identity/key pairs, without the MCP endpoint service key.
"""

from __future__ import annotations

import hmac
import json
import logging

from mcp.server.auth.routes import build_metadata, create_auth_routes
from mcp.server.auth.settings import ClientRegistrationOptions, RevocationOptions
from pydantic import AnyHttpUrl
from starlette.applications import Starlette
from starlette.requests import Request
from starlette.responses import JSONResponse, PlainTextResponse, RedirectResponse, Response
from starlette.routing import Route

from .devices import Devices
from .config import SCOPE, StubConfig
from .provider import StubProvider
from .store import Store
from .tokens import GRANT_CLAIM, IDENTITY_CLAIM, TENANT_CLAIM, TokenIssuer

log = logging.getLogger("stub_control_plane")

BANNER = ("ACS stub control plane — DEV ONLY, NOT FOR PRODUCTION. It auto-approves "
          "a fixed dev user and holds tenant signing keys in a local file.")
MAX_BODY = 16 * 1024
_NO_STORE = {"Cache-Control": "no-store", "Pragma": "no-cache"}


def _error(status: int, error: str, description: str) -> JSONResponse:
    return JSONResponse({"error": error, "error_description": description},
                        status_code=status, headers=_NO_STORE)


def create_app(cfg: StubConfig, store: Store, issuer: TokenIssuer) -> Starlette:
    devices = Devices(cfg, store, issuer)
    provider = StubProvider(cfg, store, issuer)
    issuer_url = AnyHttpUrl(cfg.issuer)
    registration = ClientRegistrationOptions(enabled=True, valid_scopes=[SCOPE],
                                             default_scopes=[SCOPE])
    revocation = RevocationOptions(enabled=True)
    metadata = build_metadata(issuer_url, None, registration, revocation)
    metadata.client_id_metadata_document_supported = True
    metadata.token_endpoint_auth_methods_supported = [
        "none", "client_secret_post", "client_secret_basic"]
    metadata_doc = {**metadata.model_dump(mode="json", exclude_none=True),
                    "jwks_uri": f"{cfg.issuer}/.well-known/jwks.json"}

    async def as_metadata(request: Request) -> Response:
        return JSONResponse(metadata_doc, headers={"Cache-Control": "public, max-age=300",
                                                   "Access-Control-Allow-Origin": "*"})

    async def jwks(request: Request) -> Response:
        return JSONResponse(issuer.jwks(), headers={"Cache-Control": "public, max-age=300",
                                                    "Access-Control-Allow-Origin": "*"})

    async def consent(request: Request) -> Response:
        # A real control plane shows a sign-in + consent page here. The stub
        # approves at once, as the fixed dev user.
        target = provider.approve(request.query_params.get("request", ""))
        if target is None:
            return PlainTextResponse("authorization request expired or unknown",
                                     status_code=400, headers=_NO_STORE)
        return RedirectResponse(target, status_code=302, headers=_NO_STORE)

    async def nats_credentials(request: Request) -> Response:
        auth = request.headers.get("authorization", "")
        if not auth.lower().startswith("bearer "):
            return _error(401, "invalid_token", "bearer access token required")
        try:
            body = await request.body()
            data = json.loads(body) if len(body) <= MAX_BODY else {}
            public_key = data.get("public_key")
        except (ValueError, AttributeError):
            return _error(400, "invalid_request", "JSON object required")
        device = devices.authenticate(request)
        if device:
            tenant, name = device["tenant"], data.get("identity")
            ident = store.identity(tenant, str(name))
            if (not ident or ident.revoked or ident.kind != "device"
                    or ident.client_id != device["id"]
                    or data.get("tenant", tenant) != tenant
                    or public_key not in ident.nats_public_keys):
                return _error(403, "access_denied", "identity/key not owned by this device")
        else:
            key = request.headers.get("x-acs-service-key", "")
            if not store.service_key or not hmac.compare_digest(key.encode(), store.service_key.encode()):
                return _error(401, "invalid_client", "service key required")
            claims = issuer.verify(auth[7:].strip(), audience=cfg.resources)
            if claims is None or claims.get("jti") in provider.revoked_jtis:
                return _error(401, "invalid_token", "access token invalid or expired")
            if SCOPE not in str(claims.get("scope", "")).split():
                return _error(403, "insufficient_scope", f"scope {SCOPE} required")
            # The grant (refresh-token family) must still be live: a stolen
            # access token can't keep minting NATS creds once the family was
            # revoked (refresh-token reuse, /revoke, identity revoked).
            # Grants are persisted, so this survives a stub restart.
            grant = store.grants.get(str(claims.get(GRANT_CLAIM, "")))
            if not provider.grant_active(grant):
                return _error(401, "invalid_token", "grant revoked or unknown")
            tenant, name = claims.get(TENANT_CLAIM), claims.get(IDENTITY_CLAIM)
            ident = store.identity(str(tenant), str(name))
        keys = cfg.tenants.get(str(tenant))
        if ident is None or ident.revoked or keys is None:
            return _error(403, "access_denied", "identity revoked or unknown")
        try:
            user_jwt, expires_at = keys.mint_user(
                identity=ident.identity, user_public_key=str(public_key),
                role=ident.role, ttl_secs=cfg.nats_ttl)
        except ValueError as e:
            return _error(400, "invalid_request", str(e))
        if public_key not in ident.nats_public_keys:
            ident.nats_public_keys = (ident.nats_public_keys + [str(public_key)])[-16:]
            store.save()
        log.info("issued NATS credentials for %s/%s (role %s)", ident.tenant,
                 ident.identity, ident.role)
        return JSONResponse({"tenant": ident.tenant, "identity": ident.identity,
                             "role": ident.role, "jwt": user_jwt,
                             "expires_at": expires_at}, headers=_NO_STORE)

    async def healthz(request: Request) -> Response:
        return JSONResponse({"ok": True, "dev_only": True})

    async def index(request: Request) -> Response:
        return PlainTextResponse(BANNER + "\n")

    routes = [r for r in create_auth_routes(
        provider, issuer_url, client_registration_options=registration,
        revocation_options=revocation)
        if getattr(r, "path", "") != "/.well-known/oauth-authorization-server"]
    routes += [
        Route("/.well-known/oauth-authorization-server", as_metadata, methods=["GET"]),
        Route("/.well-known/jwks.json", jwks, methods=["GET"]),
        Route("/consent", consent, methods=["GET"]),
        Route("/v1/credentials/nats", nats_credentials, methods=["POST"]),
        Route("/healthz", healthz, methods=["GET"]),
        Route("/", index, methods=["GET"]),
    ]
    app = Starlette(routes=routes + devices.routes())
    app.state.devices = devices
    app.state.provider = provider
    return app
