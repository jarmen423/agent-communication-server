#!/usr/bin/env python3
"""
nats-hub Cursor worker — hub-delegate + hub-session via worker_runtime.

Usage:
    python3 cursor_worker.py --identity cursor-worker-1 --repo /home/jfrie/nats
"""

import argparse
import asyncio
import os
from pathlib import Path
from typing import Any

# Load .env from repo root before Cursor SDK reads env
_env_path = Path(__file__).parent / ".env"
if _env_path.exists():
    for line in _env_path.read_text().splitlines():
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            key, _, val = line.partition("=")
            key = key.strip()
            val = val.strip().strip('"').strip("'")
            if "," in val:
                val = val.split(",")[0].strip().strip('"').strip("'")
            if key not in os.environ:
                os.environ[key] = val

from worker_runtime import WorkerConfig, run_worker


def get_api_key() -> str:
    key = os.environ.get("CURSOR_API_KEY")
    if not key:
        raise RuntimeError("CURSOR_API_KEY not set in .env or environment")
    return key.strip('"').strip("'")


class CursorBackend:
    def __init__(self, model: str, repo: str, api_key: str) -> None:
        self.model = model
        self.repo = repo
        self.api_key = api_key

    def _run_sync(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        from cursor_sdk import Agent, LocalAgentOptions
        from cursor_sdk.types import AgentOptions

        opts = LocalAgentOptions(cwd=self.repo)
        existing = ctx.get("agent_id")

        if existing:
            resume_opts = AgentOptions(model=self.model, local=opts, api_key=self.api_key)
            agent = Agent.resume(existing, options=resume_opts)
        else:
            agent = Agent.create(model=self.model, api_key=self.api_key, local=opts)

        with agent as a:
            run = a.send(prompt)
            text = run.text()
            ctx["agent_id"] = a.agent_id
            return text, ctx

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        loop = asyncio.get_event_loop()
        return await loop.run_in_executor(None, self._run_sync, prompt, dict(ctx))


async def main() -> None:
    parser = argparse.ArgumentParser(description="Cursor SDK worker for nats-hub")
    parser.add_argument("--identity", default="cursor-worker-1")
    parser.add_argument("--model", default="composer-2.5")
    parser.add_argument("--repo", default=os.getcwd())
    parser.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    parser.add_argument("--channel", default=None)
    args = parser.parse_args()

    api_key = get_api_key()
    print(f"[cursor-worker] identity={args.identity} model={args.model} repo={args.repo}")

    await run_worker(
        WorkerConfig(
            identity=args.identity,
            backend=CursorBackend(args.model, args.repo, api_key),
            nats_url=args.nats_url,
            log_prefix="cursor-worker",
            broadcast_channel=args.channel,
        )
    )


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass