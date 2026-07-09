#!/usr/bin/env python3
"""Echo worker — returns the prompt reversed. For dogfooding wave flows."""
import asyncio
import sys

from worker_backends.sdk_agent import SdkAgentBackend
from worker_runtime import WorkerConfig, run_worker


def echo_run(prompt: str, ctx: dict) -> tuple[str, dict]:
    """Trivial: reverse the prompt and return it."""
    result = f"echo: {prompt[::-1]}"
    return result, ctx


async def main():
    identity = "echo-worker-1"
    nats_url = "nats://127.0.0.1:4222"

    # Parse minimal args
    for i, arg in enumerate(sys.argv):
        if arg == "--identity" and i + 1 < len(sys.argv):
            identity = sys.argv[i + 1]
        elif arg == "--nats-url" and i + 1 < len(sys.argv):
            nats_url = sys.argv[i + 1]

    backend = SdkAgentBackend(echo_run, log_label="echo-worker")
    await run_worker(WorkerConfig(
        identity=identity,
        backend=backend,
        nats_url=nats_url,
        log_prefix="echo-worker",
    ))


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        sys.exit(0)
