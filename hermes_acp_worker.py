#!/usr/bin/env python3
"""Hermes ACP — proper JSON-RPC 2.0 backend (replaces hermes_chat_q parsing)."""
import asyncio
import sys

from worker_backends.hermes_acp import HermesAcpBackend
from worker_runtime import WorkerConfig, run_worker

if __name__ == "__main__":
    import argparse

    p = argparse.ArgumentParser()
    p.add_argument("--identity", default="hermes-worker-1")
    p.add_argument("--model", default=None, help="Model to use (e.g. anthropic/claude-sonnet-4)")
    p.add_argument("--repo", default=".")
    p.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    p.add_argument("--channel", default=None)
    args = p.parse_args()

    async def _main():
        backend = HermesAcpBackend(model=args.model, cwd=args.repo)
        await run_worker(
            WorkerConfig(
                identity=args.identity,
                backend=backend,
                nats_url=args.nats_url,
                log_prefix="hermes-acp-worker",
                broadcast_channel=args.channel,
            )
        )

    try:
        asyncio.run(_main())
    except KeyboardInterrupt:
        sys.exit(0)