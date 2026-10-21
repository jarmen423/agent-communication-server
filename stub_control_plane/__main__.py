"""Run the DEV ONLY stub control plane.

    .venv/bin/python -m stub_control_plane \\
        --port 9300 --resource http://127.0.0.1:9400/mcp \\
        --nsc-home "$OPERATOR_DIR/nsc" --tenant T_A \\
        --service-key-file /tmp/acs-dev/service.key

It refuses to listen on a non-loopback address unless
``--insecure-dev-public`` is given. To reach it from claude.ai, keep it on
loopback and put a tunnel in front (``scripts/dev/remote_mcp_demo.sh``),
with ``--issuer`` set to the tunnel's https URL.
"""

from __future__ import annotations

import argparse
import logging
import os
import secrets
import sys
import tempfile
from pathlib import Path

import uvicorn

from .app import BANNER, create_app
from .config import StubConfig, is_loopback_host
from .nats_issuer import TenantKeys, import_nsc_tenant
from .store import Store
from .tokens import TokenIssuer


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="stub_control_plane", description=BANNER)
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=9300)
    p.add_argument("--issuer", help="public base URL (default http://<host>:<port>)")
    p.add_argument("--resource", action="append", required=True,
                   help="MCP endpoint URL tokens may be issued for (repeatable)")
    p.add_argument("--tenant", default="T_A", help="the dev user's tenant (NATS account)")
    p.add_argument("--tenant-key-file", action="append", default=[],
                   help="tenant key JSON (account public key + role signing seeds)")
    p.add_argument("--nsc-home", help="import --tenant's keys from this nsc home instead")
    p.add_argument("--state-file", help="persist clients/identities/refresh tokens (0600)")
    p.add_argument("--service-key-file", required=True,
                   help="shared secret for the MCP endpoint; created (0600) if missing")
    p.add_argument("--admin-key-file",
                   help="DEV admin key for /device/service-key (default: <service-key-file>.admin; "
                        "created 0600 if missing). Separate from the MCP endpoint's service key, "
                        "so a compromised endpoint can't mint devices")
    p.add_argument("--allow-redirect-uri", action="append", default=[],
                   help="extra exact redirect URI to allow (repeatable)")
    p.add_argument("--allow-loopback-redirects", action="store_true",
                   help="allow RFC 8252 http://127.0.0.1|localhost redirect URIs")
    p.add_argument("--allow-http-cimd", action="store_true",
                   help="tests: accept http://127.0.0.1 client ID metadata URLs")
    p.add_argument("--access-ttl", type=int, default=900)
    p.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    p.add_argument("--nats-ttl", type=int, default=3600)
    p.add_argument("--access-log", action="store_true")
    p.add_argument("--insecure-dev-public", action="store_true",
                   help="allow a non-loopback --host (still DEV ONLY)")
    return p.parse_args(argv)


def _service_key(path: Path) -> str:
    if path.exists():
        key = path.read_text().strip()
        if len(key) < 32:
            raise SystemExit(f"{path}: service key too short")
        return key
    key = secrets.token_urlsafe(32)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(key + "\n")
    return key


def build(args: argparse.Namespace) -> tuple[StubConfig, Store, TokenIssuer]:
    tenants: dict[str, TenantKeys] = {}
    for path in args.tenant_key_file:
        keys = TenantKeys.load(path)
        tenants[keys.tenant] = keys
    if args.nsc_home:
        with tempfile.TemporaryDirectory() as tmp:
            out = import_nsc_tenant(args.nsc_home, args.tenant, Path(tmp) / "tenant.json")
            tenants[args.tenant] = TenantKeys.load(out)
    issuer_url = args.issuer or f"http://{args.host}:{args.port}"
    cfg = StubConfig(
        issuer=issuer_url, resources=args.resource, tenant=args.tenant, tenants=tenants,
        extra_redirect_uris=args.allow_redirect_uri,
        allow_loopback_redirects=args.allow_loopback_redirects,
        allow_http_cimd=args.allow_http_cimd, access_ttl=args.access_ttl,
        nats_ttl=args.nats_ttl, nats_url=args.nats_url)
    store = Store(args.state_file)
    if store.signing_key_pem:
        issuer = TokenIssuer.from_pem(cfg.issuer, store.signing_key_pem)
    else:
        issuer = TokenIssuer(cfg.issuer)
        store.signing_key_pem = issuer.private_pem()
    store.service_key = _service_key(Path(args.service_key_file))
    store.admin_key = _service_key(Path(args.admin_key_file or f"{args.service_key_file}.admin"))
    store.save()
    return cfg, store, issuer


def main(argv: list[str] | None = None) -> None:
    args = parse_args(argv)
    if not is_loopback_host(args.host) and not args.insecure_dev_public:
        raise SystemExit(f"refusing to listen on non-loopback {args.host!r}: this stub is "
                         "DEV ONLY. Use a tunnel, or pass --insecure-dev-public.")
    logging.basicConfig(level=logging.INFO, format="[stub-cp] %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)
    print(f"[stub-cp] {BANNER}", file=sys.stderr)
    cfg, store, issuer = build(args)
    app = create_app(cfg, store, issuer)
    print(f"[stub-cp] issuer {cfg.issuer}; resources {', '.join(cfg.resources)}; "
          f"dev user {cfg.dev_user} in tenant {cfg.tenant}", file=sys.stderr)
    uvicorn.run(app, host=args.host, port=args.port, log_level="warning",
                access_log=args.access_log)


if __name__ == "__main__":
    main()
