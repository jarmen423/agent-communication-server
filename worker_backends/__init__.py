"""
Worker backend types for nats-hub.

| Type | Examples | Mechanism |
|------|----------|-----------|
| **HeadlessCli** | agy, hermes chat -q, grok -p | subprocess, prompt on argv |
| **SdkAgent** | Cursor SDK | in-process API; blocking work in thread pool |
| **AcpAgent** | hermes acp, grok agent stdio | protocol adapter over JSON-RPC |
| **StdinCli** | hub-worker --execute | Rust; prompt on stdin, one-shot |

All types implement WorkerBackend.run(prompt, ctx) -> (text, ctx).
"""

from worker_backends.headless_cli import HeadlessCliBackend, HeadlessCliSpec
from worker_backends.sdk_agent import SdkAgentBackend

__all__ = [
    "HeadlessCliBackend",
    "HeadlessCliSpec",
    "SdkAgentBackend",
    "agy_spec",
    "hermes_spec",
    "grok_spec",
]


def agy_spec(**kwargs) -> HeadlessCliSpec:
    from worker_backends.presets import agy_spec as _agy

    return _agy(**kwargs)


def hermes_spec(**kwargs) -> HeadlessCliSpec:
    from worker_backends.presets import hermes_spec as _hermes

    return _hermes(**kwargs)


def grok_spec(**kwargs) -> HeadlessCliSpec:
    from worker_backends.presets import grok_spec as _grok

    return _grok(**kwargs)
