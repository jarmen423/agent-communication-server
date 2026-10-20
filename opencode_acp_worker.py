#!/usr/bin/env python3
"""OpenCode ACP worker — JSON-RPC 2.0 stdio backend for the OpenCode CLI.

Spawns `opencode acp`, drives the ACP session lifecycle, and relays turns
through the nats-hub worker runtime. Sessions persist across messages via
ctx["opencode_acp_session_id"].
"""
import argparse
import asyncio
import os
import sys

from worker_backends.opencode_acp import OpencodeAcpBackend
from worker_backends.proc import install_worker_signal_handlers
from worker_runtime import WorkerConfig, run_worker


def _parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="nats-hub worker backed by OpenCode ACP")
    p.add_argument("--identity", default="opencode-acp-worker-1")
    p.add_argument("--model", default=None, help="e.g. anthropic/claude-sonnet-4.5")
    p.add_argument("--provider", default=None, help="OpenCode provider id (e.g. anthropic)")
    p.add_argument("--opencode-bin", default=None, help="path to opencode binary (default: $OPENCODE_BIN or PATH)")
    p.add_argument("--repo", default=".")
    p.add_argument("--nats-url", default=os.environ.get("NATS_URL", "nats://127.0.0.1:4222"))
    p.add_argument("--channel", default=None)
    p.add_argument("--timeout", type=float, default=900.0, help="per-prompt ACP timeout seconds")
    p.add_argument("--permission-policy", default="allow_always",
                   choices=["allow_once", "allow_always", "reject"],
                   help="how session/request_permission is answered (picked by option kind)")
    return p.parse_args()


async def _main() -> None:
    args = _parse_args()
    backend = OpencodeAcpBackend(
        model=args.model,
        provider=args.provider,
        opencode_cmd=args.opencode_bin,
        cwd=args.repo,
        request_timeout_sec=args.timeout,
        always_approve=args.permission_policy != "reject",
        permission_policy=args.permission_policy,
    )
    await run_worker(
        WorkerConfig(
            identity=args.identity,
            backend=backend,
            nats_url=args.nats_url,
            log_prefix="opencode-acp-worker",
            broadcast_channel=args.channel,
        )
    )


if __name__ == "__main__":
    install_worker_signal_handlers()  # SIGTERM also stops the CLI's process group
    try:
        asyncio.run(_main())
    except KeyboardInterrupt:
        sys.exit(0)