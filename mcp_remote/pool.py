"""NATS connections of the remote MCP endpoint, one per (tenant, identity).

The endpoint connects into each tenant's NATS account *as* the identity
the access token names, with that identity's own user credentials, so
nats-server enforces the identity's permissions (operator mode, scoped
role keys). Credentials are obtained like a device would (docs/HOSTED_PLAN.md
§3): this process generates the user nkey, keeps the seed in memory only,
and sends the public key with the caller's access token to the control
plane, which returns a user JWT signed for that identity.

Everything here is a cache. Losing it (restart, another replica) costs one
credential request and one connect; no answer depends on it. Idle
connections are drained after ``idle_secs``.
"""

from __future__ import annotations

import asyncio
import base64
import logging
import os
import time
from dataclasses import dataclass, field

import httpx
import nkeys
from nats.aio.client import Client as NATSClient

import nats_connect

log = logging.getLogger("mcp_remote.pool")

REFRESH_BEFORE = 300  # replace a connection whose JWT expires within this


class CredentialError(RuntimeError):
    pass


@dataclass
class _Entry:
    seed: bytes
    public_key: str
    jwt: str = ""
    jwt_exp: float = 0.0
    nc: NATSClient | None = None
    last_used: float = field(default_factory=time.monotonic)
    lock: asyncio.Lock = field(default_factory=asyncio.Lock)


def _new_user_key() -> tuple[bytes, str]:
    seed = nkeys.encode_seed(os.urandom(32), nkeys.PREFIX_BYTE_USER)
    return seed, nkeys.from_seed(seed).public_key.decode()


class NatsPool:
    def __init__(self, *, nats_url: str, cp_url: str, service_key: str,
                 idle_secs: float = 600.0, max_conns: int = 500) -> None:
        self.nats_url = nats_url
        self.cp_url = cp_url.rstrip("/")
        self._service_key = service_key
        self.idle_secs = idle_secs
        self.max_conns = max_conns
        self._entries: dict[tuple[str, str], _Entry] = {}
        self._sweeper: asyncio.Task | None = None

    async def get(self, tenant: str, identity: str, access_token: str) -> NATSClient:
        key = (tenant, identity)
        entry = self._entries.get(key)
        if entry is None:
            seed, pub = _new_user_key()
            entry = self._entries.setdefault(key, _Entry(seed, pub))
        entry.last_used = time.monotonic()
        async with entry.lock:
            nc = entry.nc
            fresh = entry.jwt_exp - time.time() > REFRESH_BEFORE
            if nc is not None and not nc.is_closed and fresh:
                return nc
            if not fresh:
                await self._fetch_jwt(entry, access_token)
            if nc is not None and not nc.is_closed:
                asyncio.ensure_future(_drain(nc))
            entry.nc = await self._connect(identity, entry)
        await self._evict_over_cap()
        self._ensure_sweeper()
        return entry.nc

    async def _fetch_jwt(self, entry: _Entry, access_token: str) -> None:
        try:
            async with httpx.AsyncClient(timeout=10.0) as http:
                resp = await http.post(
                    f"{self.cp_url}/v1/credentials/nats",
                    json={"public_key": entry.public_key},
                    headers={"Authorization": f"Bearer {access_token}",
                             "X-ACS-Service-Key": self._service_key})
        except httpx.HTTPError as e:
            raise CredentialError(f"control plane unreachable ({type(e).__name__})") from e
        if resp.status_code != 200:
            try:
                why = resp.json().get("error_description", "")
            except ValueError:
                why = ""
            raise CredentialError(f"control plane refused NATS credentials "
                                  f"({resp.status_code}) {why}".strip())
        body = resp.json()
        entry.jwt, entry.jwt_exp = body["jwt"], float(body["expires_at"])

    async def _connect(self, identity: str, entry: _Entry) -> NATSClient:
        def jwt_cb() -> bytes:
            return entry.jwt.encode()

        def sig_cb(nonce: str) -> bytes:
            kp = nkeys.from_seed(bytearray(entry.seed))
            try:
                return base64.b64encode(kp.sign(nonce.encode()))
            finally:
                kp.wipe()

        nc = await nats_connect.connect_nats(
            self.nats_url, identity=identity, name=f"mcp-remote:{identity}",
            user_jwt_cb=jwt_cb, signature_cb=sig_cb,
            connect_timeout=5.0, max_reconnect_attempts=2, reconnect_time_wait=0.5)
        # Fail fast on the first connect (above), then ride out NATS restarts.
        nc.options["max_reconnect_attempts"] = -1
        nc.options["reconnect_time_wait"] = 2.0
        log.info("connected to NATS as %s", identity)
        return nc

    async def _evict_over_cap(self) -> None:
        if len(self._entries) <= self.max_conns:
            return
        by_age = sorted(self._entries.items(), key=lambda kv: kv[1].last_used)
        for key, entry in by_age[: len(self._entries) - self.max_conns]:
            self._entries.pop(key, None)
            if entry.nc is not None:
                asyncio.ensure_future(_drain(entry.nc))

    def _ensure_sweeper(self) -> None:
        if self._sweeper is None or self._sweeper.done():
            self._sweeper = asyncio.ensure_future(self._sweep())

    async def _sweep(self) -> None:
        while self._entries:
            await asyncio.sleep(min(60.0, self.idle_secs))
            cutoff = time.monotonic() - self.idle_secs
            for key, entry in list(self._entries.items()):
                if entry.last_used < cutoff and not entry.lock.locked():
                    self._entries.pop(key, None)
                    if entry.nc is not None:
                        await _drain(entry.nc)

    async def close(self) -> None:
        if self._sweeper is not None:
            self._sweeper.cancel()
        entries, self._entries = list(self._entries.values()), {}
        for entry in entries:
            if entry.nc is not None:
                await _drain(entry.nc)


async def _drain(nc: NATSClient) -> None:
    try:
        if not nc.is_closed:
            await nc.drain()
    except Exception:  # noqa: BLE001 - already closing
        pass
