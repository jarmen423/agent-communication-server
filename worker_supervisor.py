#!/usr/bin/env python3
"""Spawn / stop nats-hub workers on demand (visualizer first-message ensure).

Listens (request-reply) on:
  hub.worker.ensure  {identity, provider, repo?, nats_url?, model?}
  hub.worker.stop    {identity}
  hub.worker.list    {}
  hub.worker.models  {provider}            — live model list for any provider
  hub.worker.providers {}                  — catalog of model sources

Maps provider ids (from arcade AgentDock) → worker entrypoints under this repo.
Each child logs to .tools/run/workers/<identity>.log, is restarted with
bounded backoff if it crashes, and its process group is terminated when the
supervisor exits.

Run from the repo venv (see CONTRIBUTING.md):
  make setup && .venv/bin/python worker_supervisor.py
"""
from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import os
import signal
import sys
import time
from pathlib import Path
from typing import Any

try:
    from nats.aio.client import Client as NATSClient
    from nats_connect import connect_nats
except ModuleNotFoundError:
    sys.stderr.write(
        "Missing nats-py. Run `make setup`, then:\n"
        "  .venv/bin/python worker_supervisor.py\n"
    )
    sys.exit(1)

from worker_backends.supervision import (
    Child,
    RestartPolicy,
    log_path_for,
    models_request,
    providers_request,
    spawn_child,
    stop_child,
    tail_file,
    wait_log_marker,
)

REPO = Path(__file__).resolve().parent
PY = sys.executable
DEFAULT_LOG_DIR = REPO / ".tools" / "run" / "workers"
READY_TIMEOUT_SEC = 30.0

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


class Supervisor:
    def __init__(
        self, nats_url: str, repo: Path, python: str,
        nats_auth: dict[str, Any] | None = None,
        log_dir: Path | None = None,
        restart_policy: RestartPolicy | None = None,
        script_dir: Path | None = None,
    ) -> None:
        self.nats_url = nats_url
        self.repo = repo
        self.python = python
        # Auth/TLS kwargs for the supervisor's own connection. Child workers
        # inherit NATS_* env vars via create_subprocess_exec.
        self.nats_auth: dict[str, Any] = nats_auth or {}
        self.log_dir = log_dir or DEFAULT_LOG_DIR
        self.policy = restart_policy or RestartPolicy()
        self.script_dir = script_dir or REPO
        self.ready_timeout = READY_TIMEOUT_SEC
        self.nc = NATSClient()
        self.children: dict[str, Child] = {}
        self._presence_waiters: dict[str, list[asyncio.Future[bool]]] = {}
        self._restart_tasks: dict[str, asyncio.Task[None]] = {}
        self._lock = asyncio.Lock()

    async def start(self) -> None:
        connect_kwargs: dict[str, Any] = {"name": "supervisor", **self.nats_auth}
        self.nc = await connect_nats(self.nats_url, **connect_kwargs)
        for subject, cb in (
            ("hub.worker.ensure", self._on_ensure),
            ("hub.worker.stop", self._on_stop),
            ("hub.worker.list", self._on_list),
            ("hub.worker.models", self._on_models),
            ("hub.worker.providers", self._on_providers),
            ("hub.presence", self._on_presence),
        ):
            await self.nc.subscribe(subject, cb=cb)
        print(f"[supervisor] ready on {self.nats_url} (repo={self.repo}, logs={self.log_dir})",
              flush=True)

    def _cmd(self, provider: str, identity: str, model: str | None = None) -> list[str]:
        base = PROVIDER_CMDS.get(provider)
        if not base:
            raise ValueError(f"unknown provider: {provider}")
        script, *fixed = base
        argv = [self.python, str(self.script_dir / script), *fixed,
                "--identity", identity, "--repo", str(self.repo), "--nats-url", self.nats_url]
        if model:
            argv.extend(["--model", model])
        return argv

    # ── readiness ──────────────────────────────────────────────────────

    async def _on_presence(self, msg) -> None:
        try:
            env = json.loads(msg.data.decode())
        except Exception:
            return
        meta = env.get("meta") or {}
        identity = meta.get("from") or (env.get("payload") or {}).get("identity")
        if not identity:
            return
        child = self.children.get(identity)
        if child:
            child.ready = True
        for fut in self._presence_waiters.pop(identity, []):
            if not fut.done():
                fut.set_result(True)

    async def _wait_ready(self, child: Child) -> bool:
        """Ready = inbox subscribed (log marker) or first heartbeat, whichever first."""
        if child.ready:
            return True
        fut: asyncio.Future[bool] = asyncio.get_running_loop().create_future()
        self._presence_waiters.setdefault(child.identity, []).append(fut)
        marker = asyncio.ensure_future(wait_log_marker(child.log_path, child.log_offset, child.proc))
        try:
            done, _ = await asyncio.wait({fut, marker}, timeout=self.ready_timeout,
                                         return_when=asyncio.FIRST_COMPLETED)
            ok = any(t.result() for t in done if not t.cancelled() and t.exception() is None)
        finally:
            marker.cancel()
            waiters = self._presence_waiters.get(child.identity) or []
            if fut in waiters:
                waiters.remove(fut)
        if ok:
            child.ready = True
        return ok

    # ── lifecycle ──────────────────────────────────────────────────────

    async def _launch(self, identity: str, provider: str, model: str | None,
                      argv: list[str], prev: Child | None = None) -> Child:
        log_path = log_path_for(self.log_dir, identity)
        proc, offset = await spawn_child(argv, cwd=str(self.repo), log_path=log_path)
        child = Child(identity=identity, provider=provider, argv=argv, proc=proc,
                      log_path=log_path, model=model, log_offset=offset)
        if prev is not None:
            child.restarts = prev.restarts
            child.restart_times = prev.restart_times
        self.children[identity] = child
        return child

    async def _spawn(self, identity: str, provider: str, model: str | None = None) -> dict[str, Any]:
        async with self._lock:
            existing = self.children.get(identity)
            if existing and existing.alive and not existing.stopping:
                if model is None or existing.model == model:
                    return {"ok": True, "status": "already_running", "identity": identity,
                            "provider": existing.provider, "model": existing.model,
                            "pid": existing.proc.pid, "ready": existing.ready,
                            "log": str(existing.log_path)}
                print(f"[supervisor] model change for {identity}: "
                      f"{existing.model!r} → {model!r}; restarting", flush=True)
            if existing:
                await self._retire(identity)
            argv = self._cmd(provider, identity, model=model)
            print(f"[supervisor] spawn {identity} provider={provider}: {' '.join(argv)}", flush=True)
            child = await self._launch(identity, provider, model, argv)

        ready = await self._wait_ready(child)
        if not child.alive:
            # Never came up: report it instead of crash-looping in the background.
            async with self._lock:
                if self.children.get(identity) is child:
                    self.children.pop(identity, None)
            return {"ok": False, "error": "worker exited during start", "identity": identity,
                    "provider": provider, "log": str(child.log_path),
                    "log_tail": tail_file(child.log_path, 1500)}
        return {"ok": True, "status": "started" if ready else "started_pending_presence",
                "identity": identity, "provider": provider, "model": model,
                "pid": child.proc.pid, "ready": ready, "log": str(child.log_path)}

    async def _retire(self, identity: str) -> Child | None:
        """Cancel pending restarts and stop the child's process group."""
        task = self._restart_tasks.pop(identity, None)
        if task and task is not asyncio.current_task():
            task.cancel()
        child = self.children.pop(identity, None)
        if child:
            await stop_child(child)
        return child

    async def _stop(self, identity: str) -> dict[str, Any]:
        async with self._lock:
            child = await self._retire(identity)
        if not child:
            return {"ok": True, "status": "not_running", "identity": identity}
        return {"ok": True, "status": "stopped", "identity": identity, "pid": child.proc.pid}

    def check_children(self) -> None:
        """Schedule a restart for every child that died without being stopped."""
        for ident, child in list(self.children.items()):
            if child.alive or child.stopping or ident in self._restart_tasks:
                continue
            delay = self.policy.next_delay(child.restart_times)
            code = child.proc.returncode
            if delay is None:
                print(f"[supervisor] {ident} crashed (code={code}); restart budget spent "
                      f"({self.policy.max_restarts}/{self.policy.window_sec:g}s) — giving up. "
                      f"log: {child.log_path}", flush=True)
                self.children.pop(ident, None)
                continue
            print(f"[supervisor] {ident} crashed (code={code}); restarting in {delay:g}s "
                  f"(log: {child.log_path})", flush=True)
            self._restart_tasks[ident] = asyncio.ensure_future(self._restart_later(child, delay))

    async def _restart_later(self, dead: Child, delay: float) -> None:
        try:
            await asyncio.sleep(delay)
            async with self._lock:
                if self.children.get(dead.identity) is not dead:
                    return  # stopped or replaced meanwhile
                dead.restart_times.append(time.monotonic())
                dead.restarts += 1
                await self._launch(dead.identity, dead.provider, dead.model, dead.argv, prev=dead)
                print(f"[supervisor] restarted {dead.identity} (restart #{dead.restarts})",
                      flush=True)
        finally:
            if self._restart_tasks.get(dead.identity) is asyncio.current_task():
                self._restart_tasks.pop(dead.identity, None)

    async def shutdown(self) -> None:
        """Stop every child process group (SIGTERM, then SIGKILL)."""
        for task in list(self._restart_tasks.values()):
            task.cancel()
        self._restart_tasks.clear()
        children = list(self.children.values())
        self.children.clear()
        if children:
            print(f"[supervisor] stopping {len(children)} worker(s)", flush=True)
            await asyncio.gather(*(stop_child(c) for c in children), return_exceptions=True)

    # ── request handlers ───────────────────────────────────────────────

    async def _reply(self, msg, body: dict[str, Any]) -> None:
        if msg.reply:
            await self.nc.publish(msg.reply, json.dumps(body).encode())

    async def _parse(self, msg, key: str | None = None) -> dict[str, Any] | None:
        try:
            return json.loads(msg.data.decode() or "{}")
        except Exception as e:
            await self._reply(msg, {"ok": False, "error": f"bad json: {e}",
                                    **({key: []} if key else {})})
            return None

    async def _on_ensure(self, msg) -> None:
        req = await self._parse(msg)
        if req is None:
            return
        identity = (req.get("identity") or "").strip()
        provider = (req.get("provider") or "").strip().lower()
        model = (req.get("model") or "").strip() or None
        if not identity or not provider:
            await self._reply(msg, {"ok": False, "error": "identity and provider required"})
            return
        if provider not in PROVIDER_CMDS:
            await self._reply(msg, {"ok": False, "error": f"unknown provider {provider}",
                                    "known": sorted(PROVIDER_CMDS)})
            return
        try:
            result = await self._spawn(identity, provider, model=model)
        except Exception as e:
            result = {"ok": False, "error": str(e), "identity": identity, "provider": provider}
        await self._reply(msg, result)
        print(f"[supervisor] ensure → {result}", flush=True)

    async def _on_stop(self, msg) -> None:
        req = await self._parse(msg)
        if req is None:
            return
        identity = (req.get("identity") or "").strip()
        if not identity:
            await self._reply(msg, {"ok": False, "error": "identity required"})
            return
        result = await self._stop(identity)
        await self._reply(msg, result)
        print(f"[supervisor] stop → {result}", flush=True)

    async def _on_list(self, msg) -> None:
        workers = [
            {"identity": c.identity, "provider": c.provider, "model": c.model,
             "pid": c.proc.pid, "ready": c.ready, "alive": c.alive,
             "restarts": c.restarts, "log": str(c.log_path)}
            for c in self.children.values()
        ]
        await self._reply(msg, {"ok": True, "workers": workers})

    async def _on_models(self, msg) -> None:
        """Live model list for any provider via model_catalog (CLI/config/static)."""
        req = await self._parse(msg, key="models")
        if req is not None:
            await self._reply(msg, await models_request(req))

    async def _on_providers(self, msg) -> None:
        """Catalog of model sources for all known/config providers."""
        await self._reply(msg, providers_request(list(PROVIDER_CMDS)))

    async def run_forever(self) -> None:
        stop = asyncio.Event()
        loop = asyncio.get_running_loop()
        for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            with contextlib.suppress(NotImplementedError, RuntimeError):
                loop.add_signal_handler(sig, stop.set)
        try:
            await self.start()
            while not stop.is_set():
                with contextlib.suppress(asyncio.TimeoutError):
                    await asyncio.wait_for(stop.wait(), timeout=1.0)
                self.check_children()
        finally:
            await self.shutdown()
            with contextlib.suppress(Exception):
                await self.nc.close()


async def _amain(args: argparse.Namespace) -> None:
    nats_auth: dict[str, Any] = {}
    for key in ("token", "user", "password", "ca_file",
                "cert_file", "key_file", "credentials_file", "nkeys_seed"):
        v = getattr(args, key)
        if v is not None:
            nats_auth[key] = v
    if args.tls_insecure:
        nats_auth["tls_insecure"] = True

    sup = Supervisor(
        nats_url=args.nats_url,
        repo=Path(args.repo).resolve(),
        python=args.python or sys.executable,
        nats_auth=nats_auth or None,
        log_dir=Path(args.log_dir).resolve(),
        restart_policy=RestartPolicy(max_restarts=args.max_restarts),
    )
    await sup.run_forever()


def main() -> None:
    p = argparse.ArgumentParser(description="nats-hub on-demand worker supervisor")
    p.add_argument("--nats-url", default=os.environ.get("NATS_URL", "nats://127.0.0.1:4222"))
    p.add_argument("--repo", default=str(REPO))
    p.add_argument("--python", default=None, help="Python for child workers (default: this interpreter)")
    p.add_argument("--log-dir", default=str(DEFAULT_LOG_DIR), help="per-worker <identity>.log files")
    p.add_argument("--max-restarts", type=int, default=5,
                   help="crash restarts allowed per 5 minutes before giving up")
    # Auth + TLS flags. NATS_* env vars are the defaults; flags win.
    for flag, env, help_text in (
        ("--token", "NATS_TOKEN", "NATS token"),
        ("--user", "NATS_USER", "NATS username"),
        ("--password", "NATS_PASSWORD", "NATS password"),
        ("--ca-file", "NATS_CA_FILE", "CA bundle"),
        ("--cert-file", "NATS_CERT_FILE", "mTLS cert"),
        ("--key-file", "NATS_KEY_FILE", "mTLS key"),
        ("--credentials-file", "NATS_CREDENTIALS_FILE", "NATS .creds"),
        ("--nkeys-seed", "NATS_NKEYS_SEED", "NATS NKEY seed"),
    ):
        p.add_argument(flag, default=None, help=f"{help_text}. Env: {env}")
    p.add_argument("--tls-insecure", action="store_true",
                   help="Disable cert verification. Refused unless NATS_ALLOW_INSECURE=1.")
    args = p.parse_args()
    try:
        asyncio.run(_amain(args))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
