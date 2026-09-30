#!/usr/bin/env python3
"""Claude Code worker — `claude -p --output-format stream-json` per turn.

Delegations (hub-delegate / MCP) run one print-mode turn in --repo. Hub
sessions resume the same Claude conversation with `--resume <session_id>`.
Streamed assistant text, thinking and tool calls become progress events.

Safety: --permission-mode defaults to acceptEdits. bypassPermissions is only
possible with the explicit --dangerously-skip-permissions flag.

Requires the `claude` CLI installed and authenticated (`claude auth`).

  .venv/bin/python claude_worker.py --identity claude-1 --repo /path/to/repo
"""
from __future__ import annotations

import argparse
import asyncio
import os
import sys

try:
    from worker_backends.claude_code import (
        DEFAULT_PERMISSION_MODE,
        PERMISSION_MODES,
        ClaudeCodeBackend,
        ClaudeCodeConfig,
    )
    from worker_runtime import WorkerConfig, run_worker
except ModuleNotFoundError as e:  # pragma: no cover - env guidance only
    if e.name and e.name.startswith("nats"):
        sys.stderr.write("Missing nats-py. Run `make setup`, then use .venv/bin/python\n")
        sys.exit(1)
    raise


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description="nats-hub worker backed by Claude Code (claude -p)")
    p.add_argument("--identity", default="claude-worker-1")
    p.add_argument("--repo", default=os.getcwd(), help="working directory for claude (cwd)")
    p.add_argument("--model", default=None, help="alias (opus, sonnet, haiku, fable) or full model name")
    p.add_argument("--permission-mode", default=DEFAULT_PERMISSION_MODE,
                   choices=[m for m in PERMISSION_MODES if m != "bypassPermissions"],
                   help=f"claude --permission-mode (default: {DEFAULT_PERMISSION_MODE})")
    p.add_argument("--dangerously-skip-permissions", action="store_true",
                   help="use bypassPermissions (all tool calls auto-approved). Sandboxes only.")
    p.add_argument("--allowed-tools", default=None,
                   help='comma-separated tool allow-list, e.g. "Read,Edit,Bash(git *)"')
    p.add_argument("--timeout-secs", type=float, default=900.0,
                   help="per-turn limit; the claude process group is killed on timeout")
    p.add_argument("--claude-bin", default=os.environ.get("CLAUDE_BIN", "claude"))
    p.add_argument("--nats-url", default=os.environ.get("NATS_URL", "nats://127.0.0.1:4222"))
    p.add_argument("--channel", default=None, help="also accept tasks broadcast on this channel")
    return p


def build_backend(args: argparse.Namespace) -> ClaudeCodeBackend:
    tools = [t.strip() for t in (args.allowed_tools or "").split(",") if t.strip()]
    return ClaudeCodeBackend(
        ClaudeCodeConfig(
            claude_bin=args.claude_bin,
            repo=args.repo,
            model=args.model,
            permission_mode=args.permission_mode,
            dangerously_skip_permissions=args.dangerously_skip_permissions,
            allowed_tools=tools,
            timeout_sec=args.timeout_secs,
        )
    )


def main() -> None:
    args = build_parser().parse_args()
    backend = build_backend(args)
    print(f"[claude-worker] permission-mode={backend.permission_mode} repo={backend.repo}", flush=True)
    cfg = WorkerConfig(
        identity=args.identity,
        backend=backend,
        nats_url=args.nats_url,
        log_prefix="claude-worker",
        broadcast_channel=args.channel,
        extra_heartbeat={"provider": "claude", "model": args.model},
    )
    try:
        asyncio.run(run_worker(cfg))
    except KeyboardInterrupt:
        sys.exit(0)


if __name__ == "__main__":
    main()
