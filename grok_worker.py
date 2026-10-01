#!/usr/bin/env python3
"""Grok CLI worker — HeadlessCli via `grok -p` (non-interactive).

Requires the xAI Grok Build TUI binary on PATH as `grok`, or set GROK_BIN.
Smoke: grok -p "Reply with exactly: pong" --max-turns 1 --always-approve
"""
from __future__ import annotations

import argparse
import asyncio
import os
import sys
from pathlib import Path

from worker_backends.headless_cli import HeadlessCliBackend, HeadlessCliSpec
from worker_backends.proc import install_worker_signal_handlers
from worker_runtime import WorkerConfig, run_worker


def resolve_grok_bin() -> str:
    env = os.environ.get("GROK_BIN")
    if env and Path(env).exists():
        return env
    # common install locations
    candidates = [
        Path.home() / ".local/bin/grok",
        Path.home() / ".grok/downloads/grok-0.2.93-linux-x86_64",
        Path("/usr/local/bin/grok"),
    ]
    # newest grok-* binary under ~/.grok/downloads (lexicographic version sort)
    downloads = Path.home() / ".grok/downloads"
    if downloads.is_dir():
        newest = sorted(downloads.glob("grok-*-linux-x86_64"), reverse=True)
        candidates = newest + candidates
    for p in candidates:
        if p.exists() and os.access(p, os.X_OK):
            return str(p)
    return "grok"  # hope PATH works


def grok_spec(
    *,
    repo: str | Path = ".",
    model: str | None = None,
    max_turns: int = 40,
    always_approve: bool = True,
    timeout_sec: float | None = 900.0,
) -> HeadlessCliSpec:
    base: list[str] = []
    if always_approve:
        base.append("--always-approve")
    base.extend(["--max-turns", str(max_turns)])
    if model:
        base.extend(["-m", model])
    # HeadlessCli appends: [prompt_flag, prompt] → grok ... -p "<prompt>"
    return HeadlessCliSpec(
        binary=resolve_grok_bin(),
        log_label="grok-worker",
        repo=repo,
        base_argv=base,
        prompt_flag="-p",
        resume_mode="none",
        timeout_sec=timeout_sec,
    )


if __name__ == "__main__":
    p = argparse.ArgumentParser(description="nats-hub worker backed by Grok CLI")
    p.add_argument("--identity", default="grok-worker-1")
    p.add_argument("--model", default=None)
    p.add_argument("--repo", default=os.getcwd())
    p.add_argument("--nats-url", default=os.environ.get("NATS_URL", "nats://127.0.0.1:4222"))
    p.add_argument("--max-turns", type=int, default=40)
    p.add_argument("--channel", default=None)
    p.add_argument("--timeout-secs", type=float, default=900.0,
                   help="per-turn limit; the process group is killed on timeout")
    args = p.parse_args()

    spec = grok_spec(repo=args.repo, model=args.model, max_turns=args.max_turns,
                     timeout_sec=args.timeout_secs)

    async def _main() -> None:
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
