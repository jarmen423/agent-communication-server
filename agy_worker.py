#!/usr/bin/env python3
"""
Antigravity (agy) worker — hub-delegate + hub-session via worker_runtime.

Usage:
    python3 agy_worker.py --identity agy-worker-1 --repo /path/to/repo
"""

import argparse
import asyncio
import os
from pathlib import Path
from typing import Any

from worker_runtime import WorkerConfig, run_worker


class AgyBackend:
    def __init__(
        self,
        agy: str = "agy",
        repo: str | os.PathLike[str] = ".",
        model: str | None = None,
        print_timeout: str | None = None,
    ) -> None:
        self.agy = agy
        self.repo = Path(repo).resolve()
        self.model = model
        self.print_timeout = print_timeout

    def _base_cmd(self) -> list[str]:
        cmd = [self.agy]
        if self.model:
            cmd.extend(["--model", self.model])
        if self.print_timeout:
            cmd.extend(["--print-timeout", self.print_timeout])
        return cmd

    def _session_cwd(self, ctx: dict[str, Any]) -> Path:
        sid = ctx.get("_session_id", "oneshot")
        path = self.repo / ".nats-hub" / "agy-sessions" / sid
        path.mkdir(parents=True, exist_ok=True)
        return path

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        cwd = self._session_cwd(ctx) if ctx.get("_session_id") else self.repo
        cmd = self._base_cmd()
        if ctx.get("agy_has_turn"):
            cmd.append("--continue")
        cmd.extend(["-p", prompt])

        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=str(cwd),
        )
        stdout, stderr = await proc.communicate()
        out = stdout.decode().strip()
        if proc.returncode != 0:
            err = stderr.decode().strip()
            if out:
                ctx["agy_has_turn"] = True
                return out, ctx
            raise RuntimeError(f"agy exit {proc.returncode}: {err[:800]}")

        ctx["agy_has_turn"] = True
        return out or "(empty response)", ctx


async def main() -> None:
    parser = argparse.ArgumentParser(description="Antigravity worker for nats-hub")
    parser.add_argument("--identity", default="agy-worker-1")
    parser.add_argument("--agy", default="agy")
    parser.add_argument("--model", default=None)
    parser.add_argument("--repo", default=os.getcwd())
    parser.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    parser.add_argument("--print-timeout", default=None)
    parser.add_argument("--channel", default=None)
    args = parser.parse_args()

    print(f"[agy-worker] identity={args.identity} agy={args.agy} repo={args.repo}")

    await run_worker(
        WorkerConfig(
            identity=args.identity,
            backend=AgyBackend(
                agy=args.agy,
                repo=args.repo,
                model=args.model,
                print_timeout=args.print_timeout,
            ),
            nats_url=args.nats_url,
            log_prefix="agy-worker",
            broadcast_channel=args.channel,
        )
    )


if __name__ == "__main__":
    asyncio.run(main())