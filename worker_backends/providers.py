"""Provider id → worker entrypoint: the one documented entrypoint per type.

``worker_supervisor.py`` spawns these (visualizer "ensure worker"); the same
table drives ``tests/python/test_worker_entrypoints.py``, which starts every
advertised type with the exact argv the supervisor uses.

Every entrypoint accepts ``--identity --repo --nats-url [--model]`` and picks
up NATS auth from the ``NATS_*`` env vars (``nats_connect.py``).
"""

from __future__ import annotations

from pathlib import Path

# provider_id → [script, *fixed args]; python / identity / repo / nats_url /
# model are filled in at spawn time.
PROVIDER_CMDS: dict[str, list[str]] = {
    "claude": ["claude_worker.py"],
    "codex": ["codex_worker.py"],
    "grok": ["grok_acp_worker.py", "--timeout", "2400"],
    "hermes": ["hermes_acp_worker.py"],
    "echo": ["echo_worker.py"],
    "agy": ["agy_worker.py"],
    "cursor": ["cursor_worker.py"],
    "kilo": ["kilo_worker.py"],
    "kilo-acp": ["kilo_acp_worker.py"],
    "opencode": ["opencode_worker.py"],
    "opencode-acp": ["opencode_acp_worker.py"],
}


def worker_argv(
    provider: str,
    identity: str,
    *,
    python: str,
    script_dir: Path,
    repo: Path,
    nats_url: str,
    model: str | None = None,
) -> list[str]:
    """The argv that starts ``provider``'s worker. ValueError if unknown."""
    base = PROVIDER_CMDS.get(provider)
    if not base:
        raise ValueError(f"unknown provider: {provider}")
    script, *fixed = base
    argv = [python, str(script_dir / script), *fixed,
            "--identity", identity, "--repo", str(repo), "--nats-url", nats_url]
    if model:
        argv.extend(["--model", model])
    return argv
