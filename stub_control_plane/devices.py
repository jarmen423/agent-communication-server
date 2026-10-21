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

from .identity_rules import check_identity
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

    def create(self):
        device = {"id": "device-" + opaque_token(12), "tenant": self.cfg.tenant,
                  "owner": self.cfg.dev_user, "revoked": False,
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
                             "tenant": device["tenant"], "device": device["id"],
                             "nats_url": self.cfg.nats_url}, headers=HEADERS)

    async def code(self, request):
        try:
            data = await body(request)
        except ValueError:
            return error(400, "invalid_request", "device request required")
        if data.get("client_id") != "acs":
            return error(400, "invalid_client", "public client_id acs required")
        device_code, user_code = opaque_token(), secrets.token_hex(4).upper()
        self.codes[token_hash(device_code)] = {
            "user_code": user_code, "expires": time.time() + self.cfg.code_ttl,
            "approved": False, "next_poll": 0, "interval": 1}
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
            return self.issue(self.create())
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

    async def manage(self, request):
        device = self.authenticate(request)
        if device is None:
            return error(401, "invalid_token", "device token required")
        if request.method == "DELETE":
            device["revoked"] = True
            self.store.save()
            return JSONResponse({"revoked": True}, headers=HEADERS)
        identities = [vars(i) for i in self.store.identities.values()
                      if i.kind == "device" and i.client_id == device["id"]
                      and i.tenant == device["tenant"] and not i.revoked]
        return JSONResponse({"tenant": device["tenant"], "device": device["id"],
                             "identities": identities}, headers=HEADERS)

    async def identity(self, request):
        device = self.authenticate(request)
        if device is None:
            return error(401, "invalid_token", "device token required")
        if request.method == "DELETE":
            ident = self.store.identity(device["tenant"], request.path_params["identity"])
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
            if data.get("tenant", device["tenant"]) != device["tenant"]:
                return error(403, "access_denied", "foreign tenant")
        except (ValueError, TypeError):
            return error(400, "invalid_request", "valid identity, role and public user key required")
        if self.store.identity(device["tenant"], name):
            return error(409, "identity_exists", "identity name already registered")
        ident = Identity(device["tenant"], name, device["owner"], "device", role,
                         client_id=device["id"], nats_public_keys=[public])
        self.store.identities[f"{device['tenant']}/{name}"] = ident
        self.store.save()
        return JSONResponse({"identity": name, "role": role}, status_code=201, headers=HEADERS)

    def routes(self):
        return [Route("/device/code", self.code, methods=["POST"]),
                Route("/device/verify", self.verify, methods=["GET", "POST"]),
                Route("/device/token", self.token, methods=["POST"]),
                Route("/device/service-key", self.service_key, methods=["POST", "DELETE"]),
                Route("/v1/device", self.manage, methods=["GET", "DELETE"]),
                Route("/v1/device/identities", self.identity, methods=["POST"]),
                Route("/v1/device/identities/{identity}", self.identity, methods=["DELETE"])]
