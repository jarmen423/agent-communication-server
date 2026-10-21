"""Run the remote MCP endpoint.

    .venv/bin/python -m mcp_remote --port 9400 \\
        --resource-url http://127.0.0.1:9400/mcp \\
        --issuer http://127.0.0.1:9300 --nats-url "$NATS_URL" \\
        --service-key-file /tmp/acs-dev/service.key

``--resource-url`` is the public URL clients use (the tunnel's https URL in
the demo); tokens must be issued for exactly that resource. Run as many
replicas as you like with the same arguments: they share no state.
"""

from __future__ import annotations

import argparse
import logging
import os
import sys
from pathlib import Path

import uvicorn

from .app import create_app


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="mcp_remote", description=__doc__.split("\n")[0])
    p.add_argument("--host", default="127.0.0.1")
    p.add_argument("--port", type=int, default=9400)
    p.add_argument("--resource-url", required=True,
                   help="public URL of the MCP endpoint (token audience)")
    p.add_argument("--issuer", required=True, help="authorization server (control plane) URL")
    p.add_argument("--cp-url", help="control plane URL for NATS credentials (default: --issuer)")
    p.add_argument("--jwks-url", help="default: <issuer>/.well-known/jwks.json")
    p.add_argument("--nats-url", default=os.environ.get("NATS_URL", "nats://127.0.0.1:4222"))
    p.add_argument("--service-key-file", required=True,
                   help="shared secret for the control plane's credential API")
    p.add_argument("--access-log", action="store_true")
    return p.parse_args(argv)


def main(argv: list[str] | None = None) -> None:
    args = parse_args(argv)
    logging.basicConfig(level=logging.INFO, format="[mcp-remote] %(message)s")
    # httpx logs every request URL at INFO; keep it quiet.
    logging.getLogger("httpx").setLevel(logging.WARNING)
    # The MCP SDK logs every request (and stateless "session" teardown) at INFO.
    logging.getLogger("mcp").setLevel(logging.WARNING)
    service_key = Path(args.service_key_file).read_text().strip()
    app = create_app(resource_url=args.resource_url, issuer=args.issuer,
                     nats_url=args.nats_url, cp_url=args.cp_url or args.issuer,
                     service_key=service_key, jwks_url=args.jwks_url)
    print(f"[mcp-remote] serving {args.resource_url} (issuer {args.issuer}, "
          f"NATS {args.nats_url})", file=sys.stderr)
    uvicorn.run(app, host=args.host, port=args.port, log_level="warning",
                access_log=args.access_log)


if __name__ == "__main__":
    main()
