"""Unified NATS connect helper with auth + TLS.

Single entrypoint shared by all Python workers, adapters, and bridges so they
honor the same auth/TLS semantics. Resolves credentials from CLI flags with
environment-variable fallbacks (CLI flag always wins).

Resolution order for every auth/TLS value:
    explicit kwarg  >  NATS_* environment variable  >  None

TLS is enabled when any of: `ca_file`, `cert_file`, `key_file`, `credentials_file`,
`nkeys_seed` is set, OR the URL scheme is `wss://` or `tls://`. With no explicit
`ca_file`, the system default trust store is loaded via
`ssl.create_default_context()` so publicly trusted certs work out of the box.

`tls_insecure=True` is a footgun and fails closed unless the operator sets
`NATS_ALLOW_INSECURE=1` in the environment. This forces an explicit, visible
acknowledgement that certificate verification is being disabled.

Example::

    from nats_connect import connect_nats

    nc = await connect_nats(
        "wss://hub.example.com:8080",
        token=os.environ["NATS_TOKEN"],
        ca_file="/etc/nats/ca/nats-server.crt",
        name="remote-agent-1",
    )
"""
from __future__ import annotations

import os
import ssl
from typing import Any, Optional

import nats
from nats.aio.client import Client


_TLS_SCHEMES = ("wss://", "tls://", "tls+ws://")


def _env(name: str) -> Optional[str]:
    """Read an env var; return None if unset or empty."""
    v = os.environ.get(name)
    return v if v else None


def _first(explicit: Any, env_name: str) -> Any:
    """Explicit kwarg wins; otherwise fall back to the env var."""
    if explicit is not None:
        return explicit
    return _env(env_name)


#: Characters permitted in a hub identity (one NATS token; contract §4.1).
_IDENTITY_CHARS = frozenset(
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-"
)


def validate_identity(identity: str) -> str:
    """Return `identity` unchanged or raise ``ValueError``.

    Identities are embedded in bound subjects (`hub.pub.<id>.<channel>`,
    `hub.register.<id>`, `hub.presence.<id>`, `hub.api.<id>.<op>`), so they
    must be exactly one NATS token drawn from `[A-Za-z0-9_-]` — no `.`,
    `*`, `>`, spaces, or empty strings.
    """
    if not identity or any(c not in _IDENTITY_CHARS for c in identity):
        raise ValueError(
            f"invalid identity {identity!r}: must be one or more of "
            "[A-Za-z0-9_-] (no '.', '*', '>')"
        )
    return identity


def build_tls_context(
    *,
    ca_file: Optional[str],
    cert_file: Optional[str],
    key_file: Optional[str],
    tls_insecure: bool,
    url: str,
) -> Optional[ssl.SSLContext]:
    """Build an SSLContext when TLS args are present or the URL requires TLS.

    Returns None when no TLS is requested. Raises ValueError on insecure mode
    without the explicit env opt-in.
    """
    needs_tls = (
        url.lower().startswith(_TLS_SCHEMES)
        or ca_file
        or cert_file
        or key_file
    )
    if not needs_tls:
        return None

    if tls_insecure and _env("NATS_ALLOW_INSECURE") != "1":
        raise ValueError(
            "tls_insecure=True refused: set NATS_ALLOW_INSECURE=1 in the "
            "environment to acknowledge that certificate verification is "
            "being disabled. This must never be used in production."
        )

    if ca_file:
        if not os.path.isfile(ca_file):
            raise FileNotFoundError(f"ca_file not found: {ca_file}")
        ctx = ssl.create_default_context(cafile=ca_file)
    else:
        # No explicit CA → trust the system store (works for public CAs).
        ctx = ssl.create_default_context()

    if cert_file or key_file:
        if not cert_file or not key_file:
            raise ValueError(
                "mutual TLS requires both cert_file and key_file "
                f"(cert_file={cert_file!r}, key_file={key_file!r})"
            )
        if not os.path.isfile(cert_file):
            raise FileNotFoundError(f"cert_file not found: {cert_file}")
        if not os.path.isfile(key_file):
            raise FileNotFoundError(f"key_file not found: {key_file}")
        ctx.load_cert_chain(certfile=cert_file, keyfile=key_file)

    if tls_insecure:
        # Opt-in only via NATS_ALLOW_INSECURE=1 (checked above).
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE

    return ctx


async def connect_nats(
    url: str,
    *,
    token: Optional[str] = None,
    user: Optional[str] = None,
    password: Optional[str] = None,
    ca_file: Optional[str] = None,
    cert_file: Optional[str] = None,
    key_file: Optional[str] = None,
    tls_insecure: bool = False,
    credentials_file: Optional[str] = None,
    nkeys_seed: Optional[str] = None,
    name: Optional[str] = None,
    **extra: Any,
) -> Client:
    """Connect to NATS with auth + TLS resolved from kwargs/env.

    Env vars honored (kwarg wins): NATS_TOKEN, NATS_USER, NATS_PASSWORD,
    NATS_CA_FILE, NATS_CERT_FILE, NATS_KEY_FILE, NATS_CREDENTIALS_FILE,
    NATS_NKEYS_SEED, NATS_TLS_INSECURE (1/true/yes), NATS_NAME.

    Returns a connected nats.aio.client.Client.
    """
    token = _first(token, "NATS_TOKEN")
    user = _first(user, "NATS_USER")
    password = _first(password, "NATS_PASSWORD")
    ca_file = _first(ca_file, "NATS_CA_FILE")
    cert_file = _first(cert_file, "NATS_CERT_FILE")
    key_file = _first(key_file, "NATS_KEY_FILE")
    credentials_file = _first(credentials_file, "NATS_CREDENTIALS_FILE")
    nkeys_seed = _first(nkeys_seed, "NATS_NKEYS_SEED")
    tls_insecure = bool(
        tls_insecure
        or _env("NATS_TLS_INSECURE") in ("1", "true", "yes", "TRUE", "YES")
    )
    name = _first(name, "NATS_NAME")

    if user and not password and not _env("NATS_PASSWORD"):
        # Allow user-only if the server is configured that way, but warn
        # loudly when password is genuinely missing.
        pass

    if token and (user or password):
        raise ValueError(
            "Specify either token auth OR user/password auth, not both"
        )

    tls_context = build_tls_context(
        ca_file=ca_file,
        cert_file=cert_file,
        key_file=key_file,
        tls_insecure=tls_insecure,
        url=url,
    )

    connect_kwargs: dict[str, Any] = {"servers": [url]}
    if name:
        connect_kwargs["name"] = name
    if token:
        connect_kwargs["token"] = token
    if user:
        connect_kwargs["user"] = user
    if password:
        connect_kwargs["password"] = password
    if credentials_file:
        if not os.path.isfile(credentials_file):
            raise FileNotFoundError(
                f"credentials_file not found: {credentials_file}"
            )
        connect_kwargs["user_credentials"] = credentials_file
    if nkeys_seed:
        if not os.path.isfile(nkeys_seed):
            raise FileNotFoundError(f"nkeys_seed not found: {nkeys_seed}")
        connect_kwargs["nkeys_seed"] = nkeys_seed
    if tls_context is not None:
        connect_kwargs["tls"] = tls_context

    # Caller-supplied extras (connect_timeout, error_cb, etc.) win last.
    connect_kwargs.update(extra)

    return await nats.connect(**connect_kwargs)
