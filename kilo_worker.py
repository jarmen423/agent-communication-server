#!/usr/bin/env python3
"""Kilo worker — headless `kilo run` preset.

See worker_backends/presets.py for the spec.
Requires: kilo CLI installed and authenticated (`kilo auth login`).
"""
from __future__ import annotations

import argparse
import asyncio
import os
import sys

from worker_backends.headless_cli import HeadlessCliBackend
from worker_backends.presets import kilo_spec
from worker_backends.proc import install_worker_signal_handlers
from worker_runtime import WorkerConfig, run_worker


if __name__ == "__main__":
    p = argparse.ArgumentParser(description="nats-hub worker backed by Kilo CLI")
    p.add_argument("--identity", default="kilo-worker-1")
    p.add_argument("--model", default=None, help="e.g. anthropic/claude-sonnet-4.5")
    p.add_argument("--kilo-bin", default="kilo", help="path to kilo binary")
    p.add_argument("--repo", default=os.getcwd())
    p.add_argument("--nats-url", default=os.environ.get("NATS_URL", "nats://127.0.0.1:4222"))
    p.add_argument("--channel", default=None)
    args = p.parse_args()

    spec = kilo_spec(
        repo=args.repo,
        model=args.model,
        kilo_bin=args.kilo_bin,
    )
    backend = HeadlessCliBackend(spec)

    install_worker_signal_handlers()  # SIGTERM also stops the CLI's process group
    try:
        asyncio.run(
            run_worker(
                WorkerConfig(
                    identity=args.identity,
                    backend=backend,
                    nats_url=args.nats_url,
                    log_prefix="kilo-worker",
                    broadcast_channel=args.channel,
                )
            )
        )
    except KeyboardInterrupt:
        sys.exit(0)
