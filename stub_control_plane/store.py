"""State of the DEV ONLY stub control plane.

Kept in memory, optionally mirrored to a ``0600`` JSON state file so a
restarted demo keeps its clients, identities and refresh tokens. Opaque
secrets (authorization codes, refresh tokens) are stored only as SHA-256
hashes. A production control plane keeps this in Postgres
(docs/HOSTED_PLAN.md §6).

Identities are the unit the later ``acs`` connector (H-1c) extends: an
identity belongs to a tenant and an owner (the user) and records what it
was minted for (``kind``: ``connector`` here; ``device`` later) and its NATS
role. ``mint_identity`` is the one place names are chosen.
"""

from __future__ import annotations

import json
import os
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

from .identity_rules import check_identity
from .tokens import token_hash


@dataclass
class Identity:
    tenant: str
    identity: str
    owner: str
    kind: str  # "connector" (OAuth connector install); "device" is reserved for H-1c
    role: str
    client_id: str | None = None
    created_at: float = field(default_factory=time.time)
    revoked: bool = False
    nats_public_keys: list[str] = field(default_factory=list)


@dataclass
class Grant:
    """One authorization-code grant: a refresh-token family. Rotation keeps
    the family; reuse of a rotated-out refresh token revokes it."""

    family: str
    client_id: str
    subject: str
    tenant: str
    identity: str
    scopes: list[str]
    resource: str
    revoked: bool = False


@dataclass
class RefreshRecord:
    family: str
    expires_at: int
    used: bool = False


class Store:
    def __init__(self, path: str | Path | None = None) -> None:
        self.path = Path(path) if path else None
        self.devices: dict[str, dict] = {}  # device id → hashed credentials + ownership
        self.clients: dict[str, dict] = {}  # client_id → OAuthClientInformationFull JSON
        self.identities: dict[str, Identity] = {}  # "<tenant>/<identity>" → record
        self.installs: dict[str, str] = {}  # "<tenant>/<owner>/<client_id>" → identity
        self.grants: dict[str, Grant] = {}  # family → grant
        self.refresh: dict[str, RefreshRecord] = {}  # token hash → record
        self.counters: dict[str, int] = {}  # "<tenant>/<prefix>" → last n
        self.signing_key_pem: str | None = None
        self.service_key: str | None = None
        if self.path and self.path.exists():
            self._load()

    # ── identities ──

    def identity(self, tenant: str, identity: str) -> Identity | None:
        return self.identities.get(f"{tenant}/{identity}")

    def mint_identity(self, *, tenant: str, owner: str, prefix: str, kind: str,
                      role: str, client_id: str | None = None) -> Identity:
        """A new, unused, valid identity ``<prefix>-<n>`` in ``tenant``."""
        while True:
            key = f"{tenant}/{prefix}"
            self.counters[key] = self.counters.get(key, 0) + 1
            name = check_identity(f"{prefix}-{self.counters[key]}")
            if f"{tenant}/{name}" not in self.identities:
                break
        record = Identity(tenant, name, owner, kind, role, client_id)
        self.identities[f"{tenant}/{name}"] = record
        self.save()
        return record

    def connector_identity(self, *, tenant: str, owner: str, client_id: str,
                           prefix: str, role: str) -> Identity:
        """The identity of one connector install (owner + OAuth client):
        minted on first consent, reused on later ones while not revoked."""
        key = f"{tenant}/{owner}/{client_id}"
        name = self.installs.get(key)
        record = self.identity(tenant, name) if name else None
        if record is None or record.revoked:
            record = self.mint_identity(tenant=tenant, owner=owner, prefix=prefix,
                                        kind="connector", role=role, client_id=client_id)
            self.installs[key] = record.identity
            self.save()
        return record

    # ── grants and refresh tokens ──

    def add_grant(self, grant: Grant) -> None:
        self.grants[grant.family] = grant
        self.save()

    def add_refresh(self, token: str, family: str, expires_at: int) -> None:
        self.refresh[token_hash(token)] = RefreshRecord(family, expires_at)
        self.save()

    def refresh_record(self, token: str) -> RefreshRecord | None:
        return self.refresh.get(token_hash(token))

    def revoke_family(self, family: str) -> None:
        grant = self.grants.get(family)
        if grant is not None:
            grant.revoked = True
        for h in [h for h, r in self.refresh.items() if r.family == family]:
            del self.refresh[h]
        self.save()

    def purge_expired(self) -> None:
        now = time.time()
        for h in [h for h, r in self.refresh.items() if r.expires_at < now]:
            del self.refresh[h]

    # ── persistence ──

    def save(self) -> None:
        if self.path is None:
            return
        self.purge_expired()
        data: dict[str, Any] = {
            "devices": self.devices,
            "clients": self.clients,
            "identities": {k: asdict(v) for k, v in self.identities.items()},
            "installs": self.installs,
            "grants": {k: asdict(v) for k, v in self.grants.items()},
            "refresh": {k: asdict(v) for k, v in self.refresh.items()},
            "counters": self.counters,
            "signing_key_pem": self.signing_key_pem,
            "service_key": self.service_key,
        }
        tmp = self.path.with_suffix(".tmp")
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as f:
            json.dump(data, f)
        os.replace(tmp, self.path)

    def _load(self) -> None:
        data = json.loads(self.path.read_text())  # type: ignore[union-attr]
        self.devices = data.get("devices", {})
        self.clients = data.get("clients", {})
        self.identities = {k: Identity(**v) for k, v in data.get("identities", {}).items()}
        self.installs = data.get("installs", {})
        self.grants = {k: Grant(**v) for k, v in data.get("grants", {}).items()}
        self.refresh = {k: RefreshRecord(**v) for k, v in data.get("refresh", {}).items()}
        self.counters = data.get("counters", {})
        self.signing_key_pem = data.get("signing_key_pem")
        self.service_key = data.get("service_key")
