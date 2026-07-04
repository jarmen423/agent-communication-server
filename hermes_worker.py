#!/usr/bin/env python3
"""
nats-hub Hermes worker — hub-delegate + hub-session via worker_runtime.

Usage:
    python3 hermes_worker.py --identity hermes-worker-1
"""

import argparse
import asyncio
import os
import re
import shlex
from typing import Any

from worker_runtime import WorkerConfig, run_worker


class HermesBackend:
    def __init__(
        self,
        repo: str,
        model: str | None = None,
        provider: str | None = None,
        toolsets: str | None = None,
        skills: str | None = None,
        max_turns: int = 15,
    ) -> None:
        self.repo = repo
        self.model = model
        self.provider = provider
        self.toolsets = toolsets
        self.skills = skills
        self.max_turns = max_turns

    def _base_cmd(self, ctx: dict[str, Any]) -> list[str]:
        cmd = ["hermes", "chat", "-Q", "--max-turns", str(self.max_turns), "--pass-session-id"]
        if self.model:
            cmd.extend(["-m", self.model])
        if self.provider:
            cmd.extend(["--provider", self.provider])
        if self.toolsets:
            cmd.extend(["-t", self.toolsets])
        if self.skills:
            cmd.extend(["-s", self.skills])
        if ctx.get("hermes_session_id"):
            cmd.extend(["--resume", ctx["hermes_session_id"]])
        return cmd

    @staticmethod
    def _parse_output(raw: str) -> tuple[str, str | None]:
        lines = raw.strip().splitlines()
        session_id = None
        for line in lines:
            if line.startswith("session_id:"):
                session_id = line.split(":", 1)[1].strip()
                break
        response_lines = [
            line
            for line in lines
            if not line.startswith("session_id:")
            and not line.startswith("Warning:")
            and not line.startswith("⚠️")
        ]
        text = "\n".join(response_lines).strip() or raw.strip()
        if not session_id:
            m = re.search(r"session[_\s-]?id[:\s]+(\S+)", raw, re.I)
            if m:
                session_id = m.group(1)
        return text, session_id

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        cmd = self._base_cmd(ctx) + ["-q", prompt]
        print(f"[hermes-worker] exec: {' '.join(shlex.quote(c) for c in cmd[:6])} ... -q '<prompt>'")

        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=self.repo,
        )
        stdout, stderr = await proc.communicate()
        raw = stdout.decode().strip()

        if proc.returncode != 0:
            if raw:
                text, sid = self._parse_output(raw)
                if sid:
                    ctx["hermes_session_id"] = sid
                return text, ctx
            raise RuntimeError(f"hermes exit {proc.returncode}: {stderr.decode().strip()[:500]}")

        text, sid = self._parse_output(raw)
        if sid:
            ctx["hermes_session_id"] = sid
        return text, ctx


async def main() -> None:
    parser = argparse.ArgumentParser(description="Hermes Agent worker for nats-hub")
    parser.add_argument("--identity", default="hermes-worker-1")
    parser.add_argument("--model", default=None)
    parser.add_argument("--provider", default=None)
    parser.add_argument("--toolsets", default=None)
    parser.add_argument("--skills", default=None)
    parser.add_argument("--max-turns", type=int, default=15)
    parser.add_argument("--repo", default=os.getcwd())
    parser.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    parser.add_argument("--channel", default=None)
    args = parser.parse_args()

    print(f"[hermes-worker] identity={args.identity} repo={args.repo}")

    await run_worker(
        WorkerConfig(
            identity=args.identity,
            backend=HermesBackend(
                repo=args.repo,
                model=args.model,
                provider=args.provider,
                toolsets=args.toolsets,
                skills=args.skills,
                max_turns=args.max_turns,
            ),
            nats_url=args.nats_url,
            log_prefix="hermes-worker",
            broadcast_channel=args.channel,
        )
    )


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass