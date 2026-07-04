"""
Worker backend types for nats-hub.

| Type | Examples | Mechanism |
|------|----------|-----------|
| **HeadlessCli** | agy, hermes, codex -q | subprocess, prompt on argv or stdin |
| **SdkAgent** | Cursor SDK | in-process API; blocking work in thread pool |
| **AcpAgent** | (future) ACP over stdio/HTTP | protocol adapter — stub for now |
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
]

def agy_spec(**kwargs) -> HeadlessCliSpec:
    from worker_backends.presets import agy_spec as _agy
    return _agy(**kwargs)

def hermes_spec(**kwargs) -> HeadlessCliSpec:
    from worker_backends.presets import hermes_spec as _hermes
    return _hermes(**kwargs)