"""Access-token verification for the remote MCP endpoint (resource server).

Tokens are JWTs signed by the control plane (EdDSA, ``kid`` in the header)
and verified offline against its JWKS: issuer, audience (this endpoint's
resource URL, RFC 8707), expiry and the ``acs:hub`` scope. The tenant and
identity the call acts as come only from the verified ``acs_tenant`` /
``acs_identity`` claims. The JWKS is cached and refetched (at most every
30 s) when a token names an unknown key, so key rotation works.
"""

from __future__ import annotations

import asyncio
import logging
import time
from typing import Any

import httpx
import jwt
from jwt import PyJWK
from mcp.server.auth.provider import AccessToken

import nats_connect

log = logging.getLogger("mcp_remote.auth")

TENANT_CLAIM = "acs_tenant"
IDENTITY_CLAIM = "acs_identity"
JWKS_MIN_REFRESH = 30.0


class HubAccessToken(AccessToken):
    tenant: str
    identity: str


class JwksTokenVerifier:
    def __init__(self, *, issuer: str, resource: str, jwks_url: str | None = None) -> None:
        self.issuer = issuer.rstrip("/")
        self.resource = resource
        self.jwks_url = jwks_url or f"{self.issuer}/.well-known/jwks.json"
        self._keys: dict[str, Any] = {}
        self._fetched_at = 0.0
        self._lock = asyncio.Lock()

    async def _refresh(self, force: bool) -> None:
        async with self._lock:
            if not force and self._keys:
                return
            if time.monotonic() - self._fetched_at < JWKS_MIN_REFRESH and self._keys:
                return
            self._fetched_at = time.monotonic()
            try:
                async with httpx.AsyncClient(timeout=5.0) as http:
                    resp = await http.get(self.jwks_url)
                resp.raise_for_status()
                self.load_jwks(resp.json())
            except (httpx.HTTPError, ValueError, jwt.PyJWTError) as e:
                log.warning("JWKS fetch from %s failed: %s", self.jwks_url, type(e).__name__)

    def load_jwks(self, doc: dict) -> None:
        """Replace the key set with the EdDSA keys of a JWKS document."""
        keys = {}
        for jwk in doc.get("keys", []):
            if jwk.get("kid") and jwk.get("alg") == "EdDSA":
                keys[jwk["kid"]] = PyJWK.from_dict(jwk).key
        self._keys = keys
        self._fetched_at = time.monotonic()

    async def _key(self, kid: str) -> Any | None:
        if kid not in self._keys:
            await self._refresh(force=bool(self._keys))
        return self._keys.get(kid)

    async def verify_token(self, token: str) -> HubAccessToken | None:
        try:
            kid = jwt.get_unverified_header(token).get("kid")
        except jwt.PyJWTError:
            return None
        if not isinstance(kid, str):
            return None
        key = await self._key(kid)
        if key is None:
            return None
        try:
            claims = jwt.decode(
                token, key, algorithms=["EdDSA"], issuer=self.issuer,
                audience=self.resource, leeway=5,
                options={"require": ["exp", "iat", "iss", "sub", "aud"]})
        except jwt.PyJWTError:
            return None
        scopes = str(claims.get("scope", "")).split()
        tenant, identity = claims.get(TENANT_CLAIM), claims.get(IDENTITY_CLAIM)
        if not isinstance(tenant, str) or not isinstance(identity, str):
            return None
        try:
            nats_connect.validate_identity(identity)
        except ValueError:
            return None
        # jwt.decode checked aud against this resource above.
        return HubAccessToken(
            token=token, client_id=str(claims.get("client_id", "")), scopes=scopes,
            expires_at=int(claims["exp"]),
            resource=self.resource,
            subject=str(claims["sub"]), claims={"iss": claims["iss"]},
            tenant=tenant, identity=identity)
