#!/usr/bin/env python3
"""Echo worker — returns the prompt reversed. For dogfooding wave flows.

Requires the repo venv (see CONTRIBUTING.md):

  make setup
  .venv/bin/python echo_worker.py --identity echo-1
"""
import asyncio
import os
import sys

try:
    from worker_backends.sdk_agent import SdkAgentBackend
    from worker_runtime import WorkerConfig, run_worker
except ModuleNotFoundError as e:
    if e.name in ("nats", "nats.aio", "nats.aio.client"):
        sys.stderr.write(
            "Missing package 'nats-py'.\n"
            "Run `make setup`, then: .venv/bin/python echo_worker.py --identity echo-1\n"
        )
        sys.exit(1)
    raise


def echo_run(prompt: str, ctx: dict) -> tuple[str, dict]:
    """Trivial: reverse the prompt and return it."""
    result = f"echo: {prompt[::-1]}"
    return result, ctx


async def main():
    identity = "echo-worker-1"
    nats_url = os.environ.get("NATS_URL", "nats://127.0.0.1:4222")

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
