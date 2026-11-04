"""RFC 8628 and device identity management. DEV ONLY: verification auto-approves.

Device/refresh/service secrets are persisted only as hashes. Every operation
rechecks the device and its tenant; device tokens never authorize MCP installs.
"""
from __future__ import annotations

import hmac
import json
import secrets
import time
from urllib.parse import urlencode

from starlette.responses import JSONResponse, PlainTextResponse
from starlette.routing import Route

from .identity_rules import check_identity, check_provider
from .nats_issuer import check_public_key
from .store import Identity
from .tokens import TENANT_CLAIM, opaque_token, token_hash

HEADERS = {"Cache-Control": "no-store", "Pragma": "no-cache"}
DEVICE_GRANT = "urn:ietf:params:oauth:grant-type:device_code"


def error(status, code, description):
    return JSONResponse({"error": code, "error_description": description},
                        status_code=status, headers=HEADERS)


async def body(request):
    raw = await request.body()
    if len(raw) > 16384:
        raise ValueError("request too large")
    if "application/json" in request.headers.get("content-type", ""):
        data = json.loads(raw)
    else:
        data = dict(await request.form())
    if not isinstance(data, dict):
        raise ValueError("object required")
    return data


class Devices:
    def __init__(self, cfg, store, issuer):
        self.cfg, self.store, self.issuer = cfg, store, issuer
        self.codes = {}  # ephemeral, never persisted

    def create(self, name=None):
        device = {"id": "device-" + opaque_token(12), "tenant": self.cfg.tenant,
                  "owner": self.cfg.dev_user, "name": name, "revoked": False,
                  "refresh": {}, "keys": {}}
        self.store.devices[device["id"]] = device
        self.store.save()
        return device

    def authenticate(self, request):
        auth = request.headers.get("authorization", "")
        if not auth.lower().startswith("bearer "):
            return None
        claims = self.issuer.verify(auth[7:].strip(), audience=self.cfg.issuer)
        if not claims or claims.get("scope") != "acs:device":
            return None
        device = self.store.devices.get(claims.get("client_id"))
        if (not device or device["revoked"]
                or claims.get(TENANT_CLAIM) != device["tenant"]):
            return None
        return device

    def issue(self, device):
        token = self.issuer.mint(subject=device["owner"], client_id=device["id"],
                                 audience=self.cfg.issuer, scopes=["acs:device"],
                                 tenant=device["tenant"], identity="",
                                 ttl_secs=self.cfg.access_ttl)
        refresh = opaque_token()
        device["refresh"][token_hash(refresh)] = {
            "expires_at": int(time.time()) + self.cfg.refresh_ttl, "used": False}
        self.store.save()
        return JSONResponse({"access_token": token.token, "token_type": "Bearer",
                             "expires_in": self.cfg.access_ttl, "refresh_token": refresh,
                             "tenant": device["tenant"], "tenants": [t["tenant"] for t in self.memberships()], "device": device["id"],
                             "nats_url": self.cfg.nats_url}, headers=HEADERS)

    async def code(self, request):
        try:
            data = await body(request)
        except ValueError:
            return error(400, "invalid_request", "device request required")
        if data.get("client_id") != "acs":
            return error(400, "invalid_client", "public client_id acs required")
        name = data.get("device_name", data.get("client_name"))
        name = name.strip() if isinstance(name, str) else None
        if name and (len(name) > 64 or any(ord(c) < 32 or 127 <= ord(c) <= 159 for c in name)):
            name = None
        device_code, user_code = opaque_token(), secrets.token_hex(4).upper()
        self.codes[token_hash(device_code)] = {
            "user_code": user_code, "expires": time.time() + self.cfg.code_ttl,
            "approved": False, "next_poll": 0, "interval": 1, "name": name or None}
        verify = f"{self.cfg.issuer}/device/verify"
        return JSONResponse({"device_code": device_code, "user_code": user_code,
                             "verification_uri": verify,
                             "verification_uri_complete": verify + "?" + urlencode({"user_code": user_code}),
                             "expires_in": self.cfg.code_ttl, "interval": 1}, headers=HEADERS)

    async def verify(self, request):
        try:
            data = dict(request.query_params) if request.method == "GET" else await body(request)
        except ValueError:
            return error(400, "invalid_request", "user_code required")
        code = next((c for c in self.codes.values()
                     if c["user_code"] == data.get("user_code")), None)
        if not code or code["expires"] <= time.time():
            return error(400, "expired_token", "unknown or expired user code")
        code["approved"] = True
        return PlainTextResponse("DEV ONLY: approved as the fixed dev user. Return to acs.\n",
                                 headers=HEADERS)

    async def token(self, request):
        try:
            data = await body(request)
        except ValueError:
            return error(400, "invalid_request", "form or JSON required")
        grant = data.get("grant_type")
        if grant == DEVICE_GRANT:
            if data.get("client_id") != "acs":
                return error(400, "invalid_client", "public client_id acs required")
            hashed = token_hash(str(data.get("device_code", "")))
            code = self.codes.get(hashed)
            if not code or code["expires"] <= time.time():
                return error(400, "expired_token", "unknown or expired device code")
            if time.time() < code["next_poll"]:
                code["interval"] += 5
                code["next_poll"] = time.time() + code["interval"]
                return error(400, "slow_down", "poll less frequently")
            code["next_poll"] = time.time() + code["interval"]
            if not code["approved"]:
                return error(400, "authorization_pending", "open the verification URL")
            del self.codes[hashed]
            return self.issue(self.create(code.get("name")))
        field = "refresh_token" if grant == "refresh_token" else "key"
        if grant not in ("refresh_token", "service_key"):
            return error(400, "unsupported_grant_type", "unsupported device grant")
        hashed = token_hash(str(data.get(field, "")))
        for device in self.store.devices.values():
            records = device["refresh"] if field == "refresh_token" else device["keys"]
            record = records.get(hashed)
            if record is None:
                continue
            if (device["revoked"] or record.get("revoked")
                    or record.get("expires_at", float("inf")) <= time.time()):
                break
            if field == "refresh_token":
                if record["used"]:
                    device["revoked"] = True  # replay revokes the whole device
                    self.store.save()
                    break
                record["used"] = True
            return self.issue(device)
        return error(400, "invalid_grant", "device credential revoked, expired or unknown")

    async def service_key(self, request):
        # DEV ONLY admin bootstrap, gated by its own admin key: the MCP
        # endpoint's service key must not be enough to mint devices (that
        # endpoint is internet-facing). Devices can't create keys for others.
        supplied = request.headers.get("x-acs-admin-key", "")
        if not self.store.admin_key or not hmac.compare_digest(
                supplied.encode(), self.store.admin_key.encode()):
            return error(401, "invalid_client", "admin service key required")
        if request.method == "POST":
            device = self.create()
            secret = opaque_token()
            device["keys"][token_hash(secret)] = {"revoked": False}
            self.store.save()
            return JSONResponse({"device": device["id"], "key": secret,
                                 "dev_only": True}, headers=HEADERS)
        try:
            data = await body(request)
        except ValueError:
            return error(400, "invalid_request", "device required")
        device = self.store.devices.get(data.get("device"))
        if not device:
            return error(404, "not_found", "unknown device")
        for key in device["keys"].values():
            key["revoked"] = True
        device["revoked"] = True
        self.store.save()
        return JSONResponse({"revoked": True}, headers=HEADERS)

    def selected(self, request, data=None):
        tenant = (data if data is not None else request.query_params).get("tenant", self.cfg.tenant)
        return tenant if isinstance(tenant, str) and tenant in self.cfg.tenants else None

    def memberships(self):
        return [{"tenant": t, "kind": "personal" if t == self.cfg.tenant else "org",
                 "role": self.cfg.membership_roles.get(t, "owner" if t == self.cfg.tenant else "member"),
                 "handle": self.cfg.dev_user, "personal": t == self.cfg.tenant, "state": "ready"}
                for t in [self.cfg.tenant, *[t for t in self.cfg.tenants if t != self.cfg.tenant]]]

    async def tenants(self, request):
        if not self.authenticate(request):
            return error(401, "invalid_token", "device token required")
        tenant = request.path_params.get("tenant")
        if tenant is not None:
            if tenant not in self.cfg.tenants:
                return error(403, "access_denied", "foreign tenant")
            return JSONResponse({"members": [{"handle": self.cfg.dev_user,
                "role": next(t["role"] for t in self.memberships() if t["tenant"] == tenant),
                "joined_at": "2026-01-01T00:00:00Z"}]}, headers=HEADERS)
        return JSONResponse({"tenants": self.memberships()}, headers=HEADERS)

    async def manage(self, request):
        device = self.authenticate(request)
        if device is None:
            return error(401, "invalid_token", "device token required")
        if request.method == "DELETE":
            device["revoked"] = True
            self.store.save()
            return JSONResponse({"revoked": True}, headers=HEADERS)
        tenant = self.selected(request)
        if request.query_params.get("tenant") == "*":
            names = list(self.cfg.tenants)
        elif tenant is None:
            return error(403, "access_denied", "foreign tenant")
        else:
            names = [tenant]
        records = [{"tenant": t, "identities": [dict(vars(i), device=device.get("name"))
                    for i in self.store.identities.values() if i.kind == "device"
                    and i.client_id == device["id"] and i.tenant == t and not i.revoked]}
                   for t in names]
        result = {"tenants": records} if request.query_params.get("tenant") == "*" else {
            "tenant": tenant, "device": device["id"], "identities": records[0]["identities"]}
        return JSONResponse(result, headers=HEADERS)

    async def identity(self, request):
        device = self.authenticate(request)
        if device is None:
            return error(401, "invalid_token", "device token required")
        if request.method == "DELETE":
            tenant = self.selected(request)
            if tenant is None:
                return error(403, "access_denied", "foreign tenant")
            ident = self.store.identity(tenant, request.path_params["identity"])
            if not ident or ident.client_id != device["id"] or ident.kind != "device":
                return error(403, "access_denied", "identity not owned by device")
            ident.revoked = True
            self.store.save()
            return JSONResponse({"revoked": True}, headers=HEADERS)
        try:
            data = await body(request)
            name = check_identity(data.get("identity"))
            role = data.get("role", "orchestrator")
            if role not in ("worker", "orchestrator"):
                raise ValueError("role must be worker or orchestrator")
            public = check_public_key(data.get("public_key"))
            provider = check_provider(data.get("provider"))
            tenant = self.selected(request, data)
            if tenant is None:
                return error(403, "access_denied", "foreign tenant")
        except (ValueError, TypeError):
            return error(400, "invalid_request", "valid identity, role, provider and public user key required")
        if self.store.identity(tenant, name):
            return error(409, "identity_exists", "identity name already registered")
        ident = Identity(tenant, name, device["owner"], "device", role,
                         client_id=device["id"], nats_public_keys=[public], provider=provider)
        self.store.identities[f"{tenant}/{name}"] = ident
        self.store.save()
        return JSONResponse({"identity": name, "role": role, "tenant": tenant}, status_code=201, headers=HEADERS)

    def routes(self):
        return [Route("/v1/tenants", self.tenants, methods=["GET"]),
                Route("/v1/tenants/{tenant}/members", self.tenants, methods=["GET"]),
                Route("/device/code", self.code, methods=["POST"]),
                Route("/device/verify", self.verify, methods=["GET", "POST"]),
                Route("/device/token", self.token, methods=["POST"]),
                Route("/device/service-key", self.service_key, methods=["POST", "DELETE"]),
                Route("/v1/device", self.manage, methods=["GET", "DELETE"]),
                Route("/v1/device/identities", self.identity, methods=["POST"]),
                Route("/v1/device/identities/{identity}", self.identity, methods=["DELETE"])]
