#!/usr/bin/env python3
"""Grok ACP worker — long-lived `grok agent stdio` JSON-RPC backend.

Preferred over grok_worker.py (headless -p) when you want:
  - persistent ACP sessions across hub-session turns
  - streaming message chunks
  - proper authenticate / session lifecycle

Requires: authenticated Grok (`grok login`) or XAI_API_KEY.
"""
from __future__ import annotations

import argparse
import asyncio
import os
import sys

from worker_backends.grok_acp import GrokAcpBackend
from worker_runtime import WorkerConfig, run_worker


if __name__ == "__main__":
    p = argparse.ArgumentParser(description="nats-hub worker backed by Grok ACP stdio")
    p.add_argument("--identity", default="grok-acp-worker-1")
    p.add_argument("--model", default=None, help="e.g. grok-4.5 or grok-composer-2.5-fast")
    p.add_argument("--repo", default=os.getcwd())
    p.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    p.add_argument("--channel", default=None)
    p.add_argument("--timeout", type=float, default=900.0, help="per-prompt ACP timeout seconds")
    args = p.parse_args()

    async def _main() -> None:
        backend = GrokAcpBackend(
            cwd=args.repo,
            model=args.model,
            request_timeout_sec=args.timeout,
            always_approve=True,
        )
        await run_worker(
            WorkerConfig(
                identity=args.identity,
                backend=backend,
                nats_url=args.nats_url,
                log_prefix="grok-acp-worker",
                broadcast_channel=args.channel,
            )
        )

    try:
        asyncio.run(_main())
    except KeyboardInterrupt:
        sys.exit(0)
