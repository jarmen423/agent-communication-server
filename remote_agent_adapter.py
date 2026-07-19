#!/usr/bin/env python3
"""Remote agent adapter — connect a worker to the hub over WebSocket.

This is the "distributed agent teams" entrypoint. The adapter runs on any
machine, connects to the NATS bus via ws:// or wss://, and joins as a full
participant: inbox routing, stateful sessions, waves, heartbeats.

The remote machine needs only:
  - Python 3.10+
  - nats-py (pip install nats-py)
  - A way to run the agent (CLI binary, ACP server, SDK, or a callable script)

It does NOT need the NATS server binary, hub-server, or SurrealDB.

Usage (generic shell-command backend):

    python3 remote_agent_adapter.py \
        --identity remote-worker-1 \
        --nats-url ws://hub-host:8080 \
        --execute "my-agent-cli --prompt"

Usage (Python backend via import path):

    python3 remote_agent_adapter.py \
        --identity kilo-worker-1 \
        --nats-url ws://hub-host:8080 \
        --backend kilo

The adapter wraps worker_runtime.run_worker() — the same runtime used by
local workers — so session/wave/event semantics are identical.

See: docs/WORKER_BACKENDS.md, docs/REMOTE_AGENTS.md
"""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import os
import sys
from pathlib import Path
from typing import Any

logger = logging.getLogger("remote-adapter")

# Ensure we can import worker_runtime from the same directory
_REPO = Path(__file__).resolve().parent
if str(_REPO) not in sys.path:
    sys.path.insert(0, str(_REPO))

from worker_backends.headless_cli import HeadlessCliBackend, HeadlessCliSpec
from worker_runtime import WorkerBackend, WorkerConfig, run_worker


# ── Shell command backend ──────────────────────────────────────────


class ShellBackend:
    """Generic backend: run a shell command per prompt, return stdout.

    The prompt is appended as the last argument (or substituted via $PROMPT).
    No session resume support — each turn is a fresh process.
    """

    def __init__(self, command: str, cwd: str | None = None, timeout: float = 600.0) -> None:
        self.command = command
        self.cwd = cwd or os.getcwd()
        self.timeout = timeout

    async def run(self, prompt: str, ctx: dict[str, Any]) -> tuple[str, dict[str, Any]]:
        import shlex

        # If command contains $PROMPT, substitute; otherwise append
        if "$PROMPT" in self.command:
            full_cmd = self.command.replace("$PROMPT", shlex.quote(prompt))
        else:
            full_cmd = f"{self.command} {shlex.quote(prompt)}"

        logger.info("[shell-backend] exec: %s", full_cmd[:120])
        proc = await asyncio.create_subprocess_shell(
            full_cmd,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            cwd=self.cwd,
        )
        try:
            stdout, stderr = await asyncio.wait_for(proc.communicate(), timeout=self.timeout)
        except asyncio.TimeoutError:
            proc.kill()
            raise RuntimeError(f"shell backend timed out after {self.timeout}s")

        text = stdout.decode(errors="replace").strip()
        if proc.returncode != 0 and not text:
            err = stderr.decode(errors="replace").strip()[:800]
            raise RuntimeError(f"shell backend exit {proc.returncode}: {err}")
        return text, ctx


# ── Named backend registry ─────────────────────────────────────────
# Each named backend is a factory function that builds a WorkerBackend
# from CLI args. Add new agent integrations here.

NAMED_BACKENDS: dict[str, str] = {
    "shell": "Generic shell command backend",
    "kilo": "Kilo CLI (kilo run --format json --auto)",
    "opencode": "OpenCode CLI (opencode run)",
}


def build_backend(args: argparse.Namespace) -> WorkerBackend:
    """Construct the appropriate backend from parsed args."""
    backend_name = args.backend

    if backend_name == "shell":
        if not args.execute:
            raise ValueError("--execute is required when --backend shell")
        return ShellBackend(command=args.execute, cwd=args.repo, timeout=args.timeout)

    elif backend_name == "kilo":
        from worker_backends.presets import kilo_spec

        spec = kilo_spec(
            repo=args.repo,
            model=args.model,
            kilo_bin=args.kilo_bin or "kilo",
        )
        return HeadlessCliBackend(spec)

    elif backend_name == "opencode":
        from worker_backends.presets import opencode_spec

        spec = opencode_spec(
            repo=args.repo,
            model=args.model,
            opencode_bin=args.opencode_bin or "opencode",
        )
        return HeadlessCliBackend(spec)

    else:
        raise ValueError(
            f"Unknown backend '{backend_name}'. Available: {sorted(NAMED_BACKENDS)}"
        )


def main() -> None:
    logging.basicConfig(
        level=logging.DEBUG if os.environ.get("DEBUG") else logging.INFO,
        format="[%(name)s] %(message)s",
    )

    p = argparse.ArgumentParser(
        description="Remote agent adapter — connect a worker to nats-hub over WebSocket",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Generic shell command
  python3 remote_agent_adapter.py --identity worker-1 \\
      --nats-url ws://hub:8080 --backend shell --execute "echo"

  # Kilo CLI
  python3 remote_agent_adapter.py --identity kilo-1 \\
      --nats-url ws://hub:8080 --backend kilo --model anthropic/claude-sonnet-4.5

  # OpenCode CLI
  python3 remote_agent_adapter.py --identity opencode-1 \\
      --nats-url ws://hub:8080 --backend opencode
        """,
    )
    p.add_argument("--identity", required=True, help="Worker identity on the bus")
    p.add_argument(
        "--nats-url",
        default=os.environ.get("NATS_URL", "ws://localhost:8080"),
        help="NATS WebSocket URL (ws:// or wss://). Default: ws://localhost:8080",
    )
    p.add_argument(
        "--backend",
        default="shell",
        choices=sorted(NAMED_BACKENDS),
        help="Backend type. Default: shell",
    )
    p.add_argument("--repo", default=os.getcwd(), help="Working directory for the agent")
    p.add_argument("--model", default=None, help="Model override (for named backends)")
    p.add_argument("--execute", default=None, help="Shell command (shell backend only)")
    p.add_argument("--kilo-bin", default=None, help="Path to kilo binary (default: kilo)")
    p.add_argument("--opencode-bin", default=None, help="Path to opencode binary")
    p.add_argument("--timeout", type=float, default=600.0, help="Per-prompt timeout (seconds)")
    p.add_argument("--channel", default=None, help="Broadcast channel to also subscribe to")

    # ── Auth + TLS ────────────────────────────────────────────────────
    # All optional. Env vars (NATS_TOKEN, NATS_USER, ...) are the defaults;
    # any CLI flag here wins over the env var. See nats_connect.connect_nats.
    auth = p.add_argument_group(
        "authentication & TLS",
        "Options forwarded to nats_connect.connect_nats(). "
        "Each has a NATS_* env var fallback (CLI flag wins).",
    )
    auth.add_argument(
        "--token",
        default=None,
        help="NATS token auth. Env: NATS_TOKEN",
    )
    auth.add_argument(
        "--user",
        default=None,
        help="NATS username. Env: NATS_USER",
    )
    auth.add_argument(
        "--password",
        default=None,
        help="NATS password. Env: NATS_PASSWORD",
    )
    auth.add_argument(
        "--ca-file",
        default=None,
        help="CA bundle file for verifying the server cert. Env: NATS_CA_FILE",
    )
    auth.add_argument(
        "--cert-file",
        default=None,
        help="Client cert (mTLS). Requires --key-file. Env: NATS_CERT_FILE",
    )
    auth.add_argument(
        "--key-file",
        default=None,
        help="Client key (mTLS). Requires --cert-file. Env: NATS_KEY_FILE",
    )
    auth.add_argument(
        "--credentials-file",
        default=None,
        help="NATS .creds file (JWT/NKEY). Env: NATS_CREDENTIALS_FILE",
    )
    auth.add_argument(
        "--nkeys-seed",
        default=None,
        help="NATS NKEY seed file. Env: NATS_NKEYS_SEED",
    )
    auth.add_argument(
        "--tls-insecure",
        action="store_true",
        help="Disable cert verification. Refused unless NATS_ALLOW_INSECURE=1. "
        "Env: NATS_TLS_INSECURE=1",
    )
    args = p.parse_args()

    try:
        backend = build_backend(args)
    except Exception as e:
        print(f"ERROR: {e}", file=sys.stderr)
        sys.exit(1)

    print(
        f"[remote-adapter] identity={args.identity} backend={args.backend} "
        f"nats_url={args.nats_url}",
        flush=True,
    )

    # Build auth/TLS kwargs for nats_connect.connect_nats(). Only non-None
    # values are forwarded; connect_nats resolves env-var fallbacks itself.
    nats_auth: dict[str, Any] = {}
    for key in (
        "token",
        "user",
        "password",
        "ca_file",
        "cert_file",
        "key_file",
        "credentials_file",
        "nkeys_seed",
    ):
        v = getattr(args, key)
        if v is not None:
            nats_auth[key] = v
    if args.tls_insecure:
        nats_auth["tls_insecure"] = True

    cfg = WorkerConfig(
        identity=args.identity,
        backend=backend,
        nats_url=args.nats_url,
        log_prefix=f"remote:{args.identity}",
        broadcast_channel=args.channel,
        nats_auth=nats_auth or None,
    )

    try:
        asyncio.run(run_worker(cfg))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
