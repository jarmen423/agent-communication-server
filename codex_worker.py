#!/usr/bin/env python3
"""Codex worker — `codex exec --json` per turn.

Delegations run one non-interactive Codex turn in --repo. Hub sessions resume
the same Codex thread with `codex exec resume <thread_id>`. JSONL items
(reasoning, commands, file changes, messages) become progress events.

Safety: --sandbox defaults to workspace-write. The Codex bypass mode
(--dangerously-bypass-approvals-and-sandbox) is only used when that exact
flag is passed to this worker.

Requires the `codex` CLI installed and logged in (`codex login`).

  .venv/bin/python codex_worker.py --identity codex-1 --repo /path/to/repo
"""
from __future__ import annotations

import argparse
import asyncio
import os
import sys

try:
    from worker_backends.codex_cli import DEFAULT_SANDBOX, SANDBOX_MODES, CodexBackend, CodexConfig
    from worker_runtime import WorkerConfig, run_worker
except ModuleNotFoundError as e:  # pragma: no cover - env guidance only
    if e.name and e.name.startswith("nats"):
        sys.stderr.write("Missing nats-py. Run `make setup`, then use .venv/bin/python\n")
        sys.exit(1)
    raise


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description="nats-hub worker backed by Codex (codex exec --json)")
    p.add_argument("--identity", default="codex-worker-1")
    p.add_argument("--repo", default=os.getcwd(), help="working root for codex (-C / cwd)")
    p.add_argument("--model", default=None, help="e.g. a slug from `codex debug models`")
    p.add_argument("--sandbox", "-s", default=DEFAULT_SANDBOX, choices=list(SANDBOX_MODES),
                   help=f"codex sandbox policy (default: {DEFAULT_SANDBOX})")
    p.add_argument("--skip-git-repo-check", action="store_true",
                   help="allow --repo to be outside a git repository")
    p.add_argument("--dangerously-bypass-approvals-and-sandbox", action="store_true",
                   dest="dangerously_bypass",
                   help="no sandbox, no approvals. Only for externally sandboxed hosts.")
    p.add_argument("--timeout-secs", type=float, default=900.0,
                   help="per-turn limit; the codex process group is killed on timeout")
    p.add_argument("--codex-bin", default=os.environ.get("CODEX_BIN", "codex"))
    p.add_argument("--nats-url", default=os.environ.get("NATS_URL", "nats://127.0.0.1:4222"))
    p.add_argument("--channel", default=None, help="also accept tasks broadcast on this channel")
    return p


def build_backend(args: argparse.Namespace) -> CodexBackend:
    return CodexBackend(
        CodexConfig(
            codex_bin=args.codex_bin,
            repo=args.repo,
            model=args.model,
            sandbox=args.sandbox,
            skip_git_repo_check=args.skip_git_repo_check,
            dangerously_bypass=args.dangerously_bypass,
            timeout_sec=args.timeout_secs,
        )
    )


def main() -> None:
    args = build_parser().parse_args()
    backend = build_backend(args)
    mode = "BYPASS (no sandbox)" if args.dangerously_bypass else args.sandbox
    print(f"[codex-worker] sandbox={mode} repo={backend.repo}", flush=True)
    cfg = WorkerConfig(
        identity=args.identity,
        backend=backend,
        nats_url=args.nats_url,
        log_prefix="codex-worker",
        broadcast_channel=args.channel,
        extra_heartbeat={"provider": "codex", "model": args.model},
    )
    try:
        asyncio.run(run_worker(cfg))
    except KeyboardInterrupt:
        sys.exit(0)


if __name__ == "__main__":
    main()
