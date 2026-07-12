#!/usr/bin/env python3
"""
Kilo ACP HTTP worker — connects a NATS worker identity to a remote
`kilo acp --port` server via the W1-A AcpHttpTransport.

Usage::

    kilo acp --port 8721 --hostname 127.0.0.1 --cwd /path/to/repo &
    python3 kilo_acp_worker.py --identity kilo-acp-1 --port 8721 --repo /path/to/repo

This is the remote-agent equivalent of `hermes_acp_worker.py`: it wraps a
provider-specific backend (KiloAcpBackend) and feeds it into the shared
`run_worker(WorkerConfig(...))` runtime. The HTTP server itself is assumed
to be started out-of-band (the `kilo acp --port ...` command), so this
worker just dials in.
"""
import asyncio
import os
import sys

from worker_backends.kilo_acp import KiloAcpBackend
from worker_runtime import WorkerConfig, run_worker


def _env_int(name: str, default: int) -> int:
    raw = os.environ.get(name)
    if not raw:
        return default
    try:
        return int(raw)
    except ValueError:
        return default


if __name__ == "__main__":
    import argparse

    p = argparse.ArgumentParser(description="Kilo ACP HTTP worker")
    p.add_argument("--identity", default="kilo-acp-worker-1",
                   help="NATS identity for this worker")
    p.add_argument("--model", default=None,
                   help="Model to use (e.g. anthropic/claude-sonnet-4)")
    p.add_argument("--port", type=int,
                   default=_env_int("KILO_ACP_PORT", 8721),
                   help="Port of the `kilo acp` HTTP server (default: 8721)")
    p.add_argument("--hostname", default="127.0.0.1",
                   help="Hostname of the `kilo acp` HTTP server")
    p.add_argument("--repo", default=".",
                   help="Working directory passed to the backend (cwd)")
    p.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    p.add_argument("--channel", default=None,
                   help="Optional broadcast channel for hub.send.* subjects")
    args = p.parse_args()

    async def _main():
        backend = KiloAcpBackend(
            port=args.port,
            hostname=args.hostname,
            cwd=args.repo,
            model=args.model,
        )
        await run_worker(
            WorkerConfig(
                identity=args.identity,
                backend=backend,
                nats_url=args.nats_url,
                log_prefix="kilo-acp-worker",
                broadcast_channel=args.channel,
            )
        )

    try:
        asyncio.run(_main())
    except KeyboardInterrupt:
        sys.exit(0)
