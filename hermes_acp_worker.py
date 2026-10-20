#!/usr/bin/env python3
"""Hermes ACP worker — long-lived `hermes acp` JSON-RPC 2.0 stdio backend.

Sessions persist across hub-session turns (and are reattached with
`session/load` after an agent restart). Needs only the core venv
(`make setup`) plus an installed, configured `hermes`.

  .venv/bin/python hermes_acp_worker.py --identity hermes-1 --repo /path/to/repo
"""
import argparse
import asyncio
import os
import sys

from worker_backends.hermes_acp import HermesAcpBackend
from worker_backends.proc import install_worker_signal_handlers
from worker_runtime import WorkerConfig, run_worker


def _parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="nats-hub worker backed by Hermes ACP stdio")
    p.add_argument("--identity", default="hermes-worker-1")
    p.add_argument("--model", default=None, help="Model to use (e.g. anthropic/claude-sonnet-4)")
    p.add_argument("--repo", default=".")
    p.add_argument("--hermes-bin", default=os.environ.get("HERMES_BIN", "hermes"))
    p.add_argument("--nats-url", default=os.environ.get("NATS_URL", "nats://127.0.0.1:4222"))
    p.add_argument("--channel", default=None)
    p.add_argument("--timeout", type=float, default=900.0, help="per-prompt ACP timeout seconds")
    p.add_argument("--permission-policy", default="allow_always",
                   choices=["allow_once", "allow_always", "reject"],
                   help="how session/request_permission is answered (picked by option kind)")
    return p.parse_args()


async def _main(args: argparse.Namespace) -> None:
    backend = HermesAcpBackend(
        model=args.model,
        cwd=args.repo,
        hermes_cmd=args.hermes_bin,
        request_timeout_sec=args.timeout,
        permission_policy=args.permission_policy,
    )
    await run_worker(
        WorkerConfig(
            identity=args.identity,
            backend=backend,
            nats_url=args.nats_url,
            log_prefix="hermes-acp-worker",
            broadcast_channel=args.channel,
            extra_heartbeat={"provider": "hermes", "model": args.model},
        )
    )


if __name__ == "__main__":
    install_worker_signal_handlers()  # SIGTERM also stops the agent's process group
    try:
        asyncio.run(_main(_parse_args()))
    except KeyboardInterrupt:
        sys.exit(0)
