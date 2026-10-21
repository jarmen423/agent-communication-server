"""OAuth 2.1 authorization server logic (DEV ONLY stub).

Plugs into the MCP Python SDK's authorization-server handlers
(``mcp.server.auth``), which already enforce the protocol shape: PKCE with
``S256`` only (the code verifier is checked at the token endpoint), redirect
URIs that match the client's registration, client authentication, RFC 8707
``resource`` parameters. This provider adds the policy:

- **Redirect allowlist** on dynamic registration (RFC 7591) and for client ID
  metadata documents (CIMD): claude.ai/claude.com and ChatGPT callbacks,
  plus configured extras and, when enabled, RFC 8252 loopback URIs.
- **CIMD**: a ``client_id`` that is an https URL is fetched as the client's
  metadata document (``client_id`` must equal the URL, public client only).
- **Consent** is a separate step (``/consent``) that auto-approves the fixed
  dev user and mints the connector install's identity.
- **Codes** are single use, 5 minutes, bound to client, redirect URI, PKCE
  challenge, scopes and resource.
- **Refresh tokens rotate**: each use returns a new one; reusing a
  rotated-out token revokes the whole grant (OAuth 2.1 §4.3.1).
"""

from __future__ import annotations

import time
import uuid

import httpx
from mcp.server.auth.provider import (
    AccessToken,
    AuthorizationCode,
    AuthorizationParams,
    AuthorizeError,
    RefreshToken,
    RegistrationError,
    TokenError,
    construct_redirect_uri,
)
from mcp.shared.auth import OAuthClientInformationFull, OAuthClientMetadata, OAuthToken
from pydantic import ValidationError

from .config import SCOPE, StubConfig, identity_prefix
from .store import Grant, Store
from .tokens import TokenIssuer, opaque_token, token_hash

CIMD_MAX_BYTES = 64 * 1024
CIMD_CACHE_SECS = 300
PENDING_TTL = 600


class StubCode(AuthorizationCode):
    tenant: str
    identity: str


class StubRefresh(RefreshToken):
    family: str
    tenant: str
    identity: str


class StubAccess(AccessToken):
    jti: str | None = None
    tenant: str | None = None
    identity: str | None = None


class StubProvider:
    def __init__(self, cfg: StubConfig, store: Store, issuer: TokenIssuer) -> None:
        self.cfg, self.store, self.issuer = cfg, store, issuer
        self._pending: dict[str, tuple[float, str, AuthorizationParams]] = {}
        self._codes: dict[str, StubCode] = {}  # code hash → code
        self._cimd: dict[str, tuple[float, OAuthClientInformationFull]] = {}
        self.revoked_jtis: set[str] = set()  # checked by /v1/credentials/nats

    # ── clients: DCR + CIMD ──

    async def get_client(self, client_id: str) -> OAuthClientInformationFull | None:
        if client_id.startswith(("https://", "http://")):
            return await self._cimd_client(client_id)
        data = self.store.clients.get(client_id)
        return OAuthClientInformationFull.model_validate(data) if data else None

    async def register_client(self, client_info: OAuthClientInformationFull) -> None:
        self._check_redirects(client_info.redirect_uris or [])
        bad = set(client_info.grant_types) - {"authorization_code", "refresh_token"}
        if bad:
            raise RegistrationError("invalid_client_metadata",
                                    f"unsupported grant types: {sorted(bad)}")
        self.store.clients[client_info.client_id or ""] = client_info.model_dump(mode="json")
        self.store.save()

    def _check_redirects(self, uris: list) -> None:
        for uri in uris:
            if not self.cfg.redirect_allowed(str(uri)):
                raise RegistrationError(
                    "invalid_redirect_uri",
                    f"redirect URI {uri} is not on this server's allowlist")

    async def _cimd_client(self, url: str) -> OAuthClientInformationFull | None:
        if not self.cfg.cimd_url_allowed(url):
            return None
        cached = self._cimd.get(url)
        if cached and cached[0] > time.time():
            return cached[1]
        try:
            async with httpx.AsyncClient(timeout=5.0, follow_redirects=False) as http:
                resp = await http.get(url, headers={"Accept": "application/json"})
            if resp.status_code != 200 or len(resp.content) > CIMD_MAX_BYTES:
                return None
            doc = resp.json()
            if not isinstance(doc, dict) or doc.get("client_id") != url:
                return None
            meta = OAuthClientMetadata.model_validate(
                {k: v for k, v in doc.items() if k != "client_id"})
        except (httpx.HTTPError, ValueError, ValidationError):
            return None
        # A metadata document can't carry a secret: public clients only.
        if meta.token_endpoint_auth_method not in (None, "none"):
            return None
        try:
            self._check_redirects(meta.redirect_uris or [])
        except RegistrationError:
            return None
        info = OAuthClientInformationFull(
            **{**meta.model_dump(), "token_endpoint_auth_method": "none"},
            client_id=url, client_id_issued_at=int(time.time()))
        self._cimd[url] = (time.time() + CIMD_CACHE_SECS, info)
        return info

    # ── authorization + consent ──

    async def authorize(self, client: OAuthClientInformationFull,
                        params: AuthorizationParams) -> str:
        if not self.cfg.redirect_allowed(str(params.redirect_uri)):
            raise AuthorizeError("invalid_request", "redirect URI not allowed")
        resource = self.cfg.resource_allowed(params.resource or self.cfg.resources[0])
        if resource is None:
            raise AuthorizeError("invalid_request",
                                 f"unknown resource {params.resource!r}")
        scopes = params.scopes or [SCOPE]
        if set(scopes) - {SCOPE}:
            raise AuthorizeError("invalid_scope", f"only scope {SCOPE!r} is supported")
        self._expire_pending()
        request_id = opaque_token(18)
        params = params.model_copy(update={"resource": resource, "scopes": scopes})
        self._pending[request_id] = (time.time() + PENDING_TTL, client.client_id or "", params)
        return f"{self.cfg.issuer}/consent?request={request_id}"

    def _expire_pending(self) -> None:
        now = time.time()
        for k in [k for k, v in self._pending.items() if v[0] < now]:
            del self._pending[k]
        for k in [k for k, v in self._codes.items() if v.expires_at < now]:
            del self._codes[k]

    def approve(self, request_id: str) -> str | None:
        """Consent: auto-approve the dev user, mint (or reuse) the connector
        install's identity, issue a code. Returns the client redirect URL."""
        entry = self._pending.pop(request_id, None)
        if entry is None or entry[0] < time.time():
            return None
        _, client_id, params = entry
        ident = self.store.connector_identity(
            tenant=self.cfg.tenant, owner=self.cfg.dev_user, client_id=client_id,
            prefix=identity_prefix(str(params.redirect_uri)), role=self.cfg.role)
        code = opaque_token(32)
        self._codes[token_hash(code)] = StubCode(
            code=code, scopes=params.scopes or [SCOPE],
            expires_at=time.time() + self.cfg.code_ttl, client_id=client_id,
            code_challenge=params.code_challenge, redirect_uri=params.redirect_uri,
            redirect_uri_provided_explicitly=params.redirect_uri_provided_explicitly,
            resource=params.resource, subject=self.cfg.dev_user,
            tenant=ident.tenant, identity=ident.identity)
        return construct_redirect_uri(str(params.redirect_uri), code=code, state=params.state)

    # ── tokens ──

    async def load_authorization_code(self, client: OAuthClientInformationFull,
                                      authorization_code: str) -> StubCode | None:
        code = self._codes.get(token_hash(authorization_code))
        if code is None or code.client_id != client.client_id:
            return None
        return code

    async def exchange_authorization_code(self, client: OAuthClientInformationFull,
                                          authorization_code: StubCode) -> OAuthToken:
        if self._codes.pop(token_hash(authorization_code.code), None) is None:
            raise TokenError("invalid_grant", "authorization code already used")
        family = uuid.uuid4().hex
        self.store.add_grant(Grant(
            family=family, client_id=client.client_id or "",
            subject=authorization_code.subject or self.cfg.dev_user,
            tenant=authorization_code.tenant, identity=authorization_code.identity,
            scopes=authorization_code.scopes,
            resource=authorization_code.resource or self.cfg.resources[0]))
        return self._issue(family, authorization_code.scopes)

    def _issue(self, family: str, scopes: list[str]) -> OAuthToken:
        grant = self.store.grants[family]
        access = self.issuer.mint(
            subject=grant.subject, client_id=grant.client_id, audience=grant.resource,
            scopes=scopes, tenant=grant.tenant, identity=grant.identity,
            ttl_secs=self.cfg.access_ttl)
        refresh = opaque_token(32)
        self.store.add_refresh(refresh, family, int(time.time()) + self.cfg.refresh_ttl)
        return OAuthToken(access_token=access.token, token_type="Bearer",
                          expires_in=self.cfg.access_ttl, scope=" ".join(scopes),
                          refresh_token=refresh)

    def grant_active(self, grant: Grant | None) -> bool:
        if grant is None or grant.revoked:
            return False
        ident = self.store.identity(grant.tenant, grant.identity)
        return ident is not None and not ident.revoked

    async def load_refresh_token(self, client: OAuthClientInformationFull,
                                 refresh_token: str) -> StubRefresh | None:
        record = self.store.refresh_record(refresh_token)
        if record is None:
            return None
        grant = self.store.grants.get(record.family)
        if record.used:
            # Reuse of a rotated-out token: someone else holds the family.
            self.store.revoke_family(record.family)
            return None
        if grant is None or grant.client_id != client.client_id or not self.grant_active(grant):
            return None
        return StubRefresh(token=refresh_token, client_id=grant.client_id,
                           scopes=grant.scopes, expires_at=record.expires_at,
                           resource=grant.resource, subject=grant.subject,
                           family=record.family, tenant=grant.tenant,
                           identity=grant.identity)

    async def exchange_refresh_token(self, client: OAuthClientInformationFull,
                                     refresh_token: StubRefresh,
                                     scopes: list[str]) -> OAuthToken:
        record = self.store.refresh_record(refresh_token.token)
        if record is None or record.used:
            raise TokenError("invalid_grant", "refresh token already used")
        if set(scopes) - set(refresh_token.scopes):
            raise TokenError("invalid_scope", "cannot widen scopes on refresh")
        record.used = True  # rotation: the presented token is now spent
        self.store.save()
        return self._issue(refresh_token.family, scopes or refresh_token.scopes)

    async def load_access_token(self, token: str) -> StubAccess | None:
        claims = self.issuer.verify(token)
        if claims is None:
            return None
        return StubAccess(token=token, client_id=claims["client_id"],
                          scopes=claims["scope"].split(), expires_at=claims["exp"],
                          resource=claims["aud"], subject=claims["sub"],
                          jti=claims["jti"], tenant=claims.get("acs_tenant"),
                          identity=claims.get("acs_identity"))

    async def revoke_token(self, token: StubAccess | StubRefresh) -> None:
        """Revocation (RFC 7009) revokes the whole grant for a refresh token.
        A revoked access token can no longer get NATS credentials, but stays
        valid at the endpoint until it expires (JWTs are verified offline;
        see tokens.py)."""
        if isinstance(token, StubRefresh):
            self.store.revoke_family(token.family)
        elif isinstance(token, StubAccess) and token.jti:
            self.revoked_jtis.add(token.jti)
