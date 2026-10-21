"""NATS user credentials for minted identities (DEV ONLY stub).

Operator mode: each tenant is a NATS account with one *scoped* signing key
per role (``scripts/dev/operator_stack.sh``, docs/OPERATOR_MODE.md). A user
JWT signed by a scoped key carries no permissions of its own; nats-server
applies the role's template with ``{{name()}}`` = the JWT ``name``. So
minting a credential means: choose the role, set ``name`` to the identity,
set ``sub`` to the caller's public user key and sign.

The caller generates the user nkey and sends only the public key; the seed
never reaches this process. Tenant signing seeds are loaded from a tenant
key file (see :func:`import_nsc_tenant`), never logged.

Production mints with the maintained NATS JWT libraries and KMS-held keys
(docs/HOSTED_PLAN.md §3); this is the smallest correct encoder for the
user claims nsc itself produces.
"""

from __future__ import annotations

import base64
import binascii
import hashlib
import json
import os
import time
from dataclasses import dataclass
from pathlib import Path

import nkeys

ROLES = ("worker", "orchestrator", "admin", "service", "hub-server")


def _b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def _b64url_decode(data: str) -> bytes:
    return base64.urlsafe_b64decode(data + "=" * (-len(data) % 4))


def check_public_key(key: str, prefix: int = nkeys.PREFIX_BYTE_USER) -> str:
    """Return ``key`` if it is a well-formed public nkey of ``prefix``
    (user by default), else raise ``ValueError``."""
    if not isinstance(key, str) or len(key) != 56:
        raise ValueError("public key must be a 56-character nkey")
    try:
        raw = base64.b32decode(key)
    except (binascii.Error, ValueError) as e:
        raise ValueError("public key is not base32") from e
    if raw[0] != prefix:
        raise ValueError("public key has the wrong nkey type")
    if nkeys.crc16(bytearray(raw[:-2])).to_bytes(2, "little") != raw[-2:]:
        raise ValueError("public key checksum mismatch")
    return key


def _jti(claims: dict) -> str:
    """nats-io/jwt: base32(sha512/256(claims JSON without jti))."""
    body = json.dumps({k: v for k, v in claims.items() if k != "jti"},
                      separators=(",", ":")).encode()
    try:
        digest = hashlib.new("sha512_256", body).digest()
    except ValueError:  # OpenSSL without sha512/256: any unique id works
        digest = hashlib.sha256(body).digest()
    return base64.b32encode(digest).rstrip(b"=").decode()


def encode_jwt(claims: dict, signer: nkeys.KeyPair) -> str:
    header = _b64url(json.dumps({"typ": "JWT", "alg": "ed25519-nkey"},
                                separators=(",", ":")).encode())
    payload = _b64url(json.dumps(claims, separators=(",", ":")).encode())
    sig = signer.sign(f"{header}.{payload}".encode())
    return f"{header}.{payload}.{_b64url(sig)}"


def decode_jwt(token: str) -> dict:
    """Claims of a NATS JWT (no verification; for tests and display)."""
    return json.loads(_b64url_decode(token.split(".")[1]))


@dataclass
class TenantKeys:
    """One tenant account: its public key and scoped signing seeds by role."""

    tenant: str
    account_public_key: str
    signing_seeds: dict[str, bytes]

    @classmethod
    def load(cls, path: str | Path) -> "TenantKeys":
        data = json.loads(Path(path).read_text())
        seeds = {role: seed.encode() for role, seed in data["signing_keys"].items()}
        for role in seeds:
            if role not in ROLES:
                raise ValueError(f"{path}: unknown role {role!r}")
        check_public_key(data["account_public_key"], nkeys.PREFIX_BYTE_ACCOUNT)
        return cls(data["tenant"], data["account_public_key"], seeds)

    def mint_user(self, *, identity: str, user_public_key: str, role: str,
                  ttl_secs: int, now: int | None = None) -> tuple[str, int]:
        """``(user_jwt, expires_at)`` for ``identity`` with ``role``."""
        seed = self.signing_seeds.get(role)
        if seed is None:
            raise ValueError(f"tenant {self.tenant} has no {role!r} signing key")
        signer = nkeys.from_seed(seed)
        iat = int(now if now is not None else time.time())
        exp = iat + int(ttl_secs)
        claims = {
            "iat": iat,
            "exp": exp,
            "iss": signer.public_key.decode(),
            "name": identity,
            "sub": check_public_key(user_public_key),
            "nats": {"pub": {}, "sub": {}, "issuer_account": self.account_public_key,
                     "type": "user", "version": 2},
        }
        claims["jti"] = _jti(claims)
        return encode_jwt(claims, signer), exp


def import_nsc_tenant(nsc_home: str | Path, account: str, out: str | Path) -> Path:
    """Write a tenant key file for ``account`` from an nsc home directory
    (``nsc -H <home>``, as created by operator_stack.sh): the account public
    key from its JWT, and each scoped signing key's seed by role. ``0600``."""
    home = Path(nsc_home)
    # nsc store layout: <home>/<operator>/accounts/<account>/<account>.jwt
    jwts = list(home.glob(f"*/accounts/{account}/{account}.jwt"))
    if len(jwts) != 1:
        raise FileNotFoundError(f"account {account!r} not found (once) under {home}")
    claims = decode_jwt(jwts[0].read_text().strip())
    seeds: dict[str, str] = {}
    for sk in claims["nats"].get("signing_keys", []):
        if not isinstance(sk, dict) or sk.get("kind") != "user_scope":
            continue
        pub = sk["key"]
        # nsc keystore layout: <home>/keys/<A>/<2nd-3rd chars>/<pub>.nk
        seed_file = home / "keys" / pub[0] / pub[1:3] / f"{pub}.nk"
        seeds[sk["role"]] = seed_file.read_text().strip()
    out = Path(out)
    fd = os.open(out, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump({"tenant": account, "account_public_key": claims["sub"],
                   "signing_keys": seeds}, f)
    return out


def new_user_seed() -> bytes:
    """A fresh user nkey seed (``SU…``); for clients of the stub."""
    return nkeys.encode_seed(os.urandom(32), nkeys.PREFIX_BYTE_USER)
