#!/usr/bin/env python3
"""OpenCode worker — headless `opencode run` preset.

See worker_backends/presets.py for the spec.
Requires: opencode CLI installed and configured.
"""
from __future__ import annotations

import argparse
import asyncio
import os

from worker_backends.headless_cli import HeadlessCliBackend
from worker_backends.presets import opencode_spec
from worker_runtime import WorkerConfig, run_worker


if __name__ == "__main__":
    p = argparse.ArgumentParser(description="nats-hub worker backed by OpenCode CLI")
    p.add_argument("--identity", default="opencode-worker-1")
    p.add_argument("--model", default=None, help="e.g. anthropic/claude-sonnet-4.5")
    p.add_argument("--opencode-bin", default="opencode", help="path to opencode binary")
    p.add_argument("--repo", default=os.getcwd())
    p.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    p.add_argument("--channel", default=None)
    args = p.parse_args()

    spec = opencode_spec(
        repo=args.repo,
        model=args.model,
        opencode_bin=args.opencode_bin,
    )
    backend = HeadlessCliBackend(spec)

    asyncio.run(
        run_worker(
            WorkerConfig(
                identity=args.identity,
                backend=backend,
                nats_url=args.nats_url,
                log_prefix="opencode-worker",
                broadcast_channel=args.channel,
            )
        )
    )
