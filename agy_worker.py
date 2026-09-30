#!/usr/bin/env python3
"""Antigravity — HeadlessCli preset. See worker_backends/presets.py."""
import asyncio
import os
import sys

from worker_backends.headless_cli import HeadlessCliBackend
from worker_backends.presets import agy_spec
from worker_backends.proc import install_worker_signal_handlers
from worker_runtime import WorkerConfig, run_worker

if __name__ == "__main__":
    import argparse

    p = argparse.ArgumentParser()
    p.add_argument("--identity", default="agy-worker-1")
    p.add_argument("--agy", default="agy")
    p.add_argument("--model", default=None)
    p.add_argument("--repo", default=os.getcwd())
    p.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    p.add_argument("--print-timeout", default=None)
    p.add_argument("--channel", default=None)
    args = p.parse_args()
    spec = agy_spec(agy=args.agy, repo=args.repo, model=args.model, print_timeout=args.print_timeout)

    async def _main():
        await run_worker(
            WorkerConfig(
                identity=args.identity,
                backend=HeadlessCliBackend(spec),
                nats_url=args.nats_url,
                log_prefix=spec.log_label,
                broadcast_channel=args.channel,
            )
        )

    install_worker_signal_handlers()  # SIGTERM also stops the CLI's process group
    try:
        asyncio.run(_main())
    except KeyboardInterrupt:
        sys.exit(0)