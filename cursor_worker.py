#!/usr/bin/env python3
"""Cursor — SdkAgent preset. See worker_backends/sdk_agent.py."""
import argparse
import asyncio
import os
import sys
from pathlib import Path
from typing import Any

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

from worker_backends.sdk_agent import SdkAgentBackend
from worker_runtime import WorkerConfig, run_worker


def get_api_key() -> str:
    key = os.environ.get("CURSOR_API_KEY")
    if not key:
        raise RuntimeError("CURSOR_API_KEY not set")
    return key.strip('"').strip("'")


def cursor_run_sync(model: str, repo: str, api_key: str):
    def _run(prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        from cursor_sdk import Agent, LocalAgentOptions
        from cursor_sdk.types import AgentOptions

        opts = LocalAgentOptions(cwd=repo)
        existing = ctx.get("agent_id")
        if existing:
            agent = Agent.resume(
                existing,
                options=AgentOptions(model=model, local=opts, api_key=api_key),
            )
        else:
            agent = Agent.create(model=model, api_key=api_key, local=opts)
        with agent as a:
            text = a.send(prompt).text()
            ctx["agent_id"] = a.agent_id
            return text, ctx

    return _run


async def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--identity", default="cursor-worker-1")
    p.add_argument("--model", default="composer-2.5")
    p.add_argument("--repo", default=os.getcwd())
    p.add_argument("--nats-url", default=os.environ.get("NATS_URL", "nats://127.0.0.1:4222"))
    p.add_argument("--channel", default=None)
    args = p.parse_args()
    api_key = get_api_key()
    backend = SdkAgentBackend(
        cursor_run_sync(args.model, args.repo, api_key),
        log_label="cursor-worker",
    )
    await run_worker(
        WorkerConfig(
            identity=args.identity,
            backend=backend,
            nats_url=args.nats_url,
            log_prefix="cursor-worker",
            broadcast_channel=args.channel,
        )
    )


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        sys.exit(0)