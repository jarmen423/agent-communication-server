"""Access tokens: Ed25519-signed JWTs (``alg: EdDSA``) and the JWKS (DEV ONLY stub).

Why JWT and not introspection: the remote MCP endpoint verifies every call
offline against the published JWKS, so the control plane is not on the hot
path of tool calls and endpoint replicas share nothing but the public key.
The cost is that a revoked access token stays usable until it expires, so
access tokens are short-lived (15 min by default) and every refresh and
every NATS credential issue rechecks the grant.

Claims:

- ``iss`` — this control plane's issuer URL; ``aud`` — the RFC 8707
  resource (the MCP endpoint URL) the token was issued for;
- ``sub`` — the user; ``client_id``; ``scope`` (space separated);
- ``acs_tenant`` / ``acs_identity`` — the tenant account and the hub
  identity minted for this connector install. The endpoint acts as exactly
  this identity; tool arguments can never change it.
"""

from __future__ import annotations

import base64
import hashlib
import json
import secrets
import time
from dataclasses import dataclass
from typing import Any

import jwt
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

TENANT_CLAIM = "acs_tenant"
IDENTITY_CLAIM = "acs_identity"


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


@dataclass
class IssuedToken:
    token: str
    jti: str
    expires_at: int


class TokenIssuer:
    """Signs access tokens with one Ed25519 key and publishes its JWKS."""

    def __init__(self, issuer: str, key: Ed25519PrivateKey | None = None) -> None:
        self.issuer = issuer
        self._key = key or Ed25519PrivateKey.generate()
        raw = self._key.public_key().public_bytes(
            serialization.Encoding.Raw, serialization.PublicFormat.Raw)
        self._public_raw = raw
        self.kid = _b64url(hashlib.sha256(raw).digest())[:16]

    # ── persistence (dev state file) ──

    def private_pem(self) -> str:
        return self._key.private_bytes(
            serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption()).decode()

    @classmethod
    def from_pem(cls, issuer: str, pem: str) -> "TokenIssuer":
        key = serialization.load_pem_private_key(pem.encode(), password=None)
        if not isinstance(key, Ed25519PrivateKey):
            raise ValueError("state file signing key is not Ed25519")
        return cls(issuer, key)

    # ── issuing / verifying ──

    def jwks(self) -> dict:
        return {"keys": [{
            "kty": "OKP", "crv": "Ed25519", "x": _b64url(self._public_raw),
            "kid": self.kid, "alg": "EdDSA", "use": "sig",
        }]}

    def mint(self, *, subject: str, client_id: str, audience: str, scopes: list[str],
             tenant: str, identity: str, ttl_secs: int) -> IssuedToken:
        now = int(time.time())
        jti = secrets.token_urlsafe(16)
        claims = {
            "iss": self.issuer, "sub": subject, "aud": audience,
            "iat": now, "nbf": now, "exp": now + int(ttl_secs), "jti": jti,
            "client_id": client_id, "scope": " ".join(scopes),
            TENANT_CLAIM: tenant, IDENTITY_CLAIM: identity,
        }
        token = jwt.encode(claims, self._key, algorithm="EdDSA",
                           headers={"kid": self.kid, "typ": "at+jwt"})
        return IssuedToken(token, jti, claims["exp"])

    def verify(self, token: str, audience: str | list[str] | None = None) -> dict[str, Any] | None:
        """Claims of a valid token issued here (any audience when
        ``audience`` is None), else None."""
        try:
            return jwt.decode(
                token, self._key.public_key(), algorithms=["EdDSA"],
                issuer=self.issuer, audience=audience,
                options={"require": ["exp", "iat", "iss", "sub", "aud", "jti"],
                         "verify_aud": audience is not None},
                leeway=5,
            )
        except jwt.PyJWTError:
            return None


def unverified_header(token: str) -> dict:
    """The JOSE header (for picking the key); never trust it otherwise."""
    try:
        return jwt.get_unverified_header(token)
    except jwt.PyJWTError:
        return {}


def opaque_token(nbytes: int = 32) -> str:
    """A random opaque token (authorization codes, refresh tokens)."""
    return secrets.token_urlsafe(nbytes)


def token_hash(token: str) -> str:
    """Stored form of an opaque token: only its SHA-256 is kept."""
    return hashlib.sha256(token.encode()).hexdigest()


def dumps(obj: Any) -> str:
    return json.dumps(obj, separators=(",", ":"), sort_keys=True)
