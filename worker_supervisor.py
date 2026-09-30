#!/usr/bin/env python3
"""Spawn / stop nats-hub workers on demand (visualizer first-message ensure).

Listens (request-reply) on:
  hub.worker.ensure  {identity, provider, repo?, nats_url?, model?}
  hub.worker.stop    {identity}
  hub.worker.list    {}
  hub.worker.models  {provider}            — live model list for any provider
  hub.worker.providers {}                  — catalog of model sources

Maps provider ids (from arcade AgentDock) → worker entrypoints under this repo.

Run from the repo venv (see CONTRIBUTING.md):
  make setup && .venv/bin/python worker_supervisor.py
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import signal
import sys
import time
from dataclasses import dataclass, field
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

REPO = Path(__file__).resolve().parent
PY = sys.executable

# provider_id → argv template (identity / repo / nats_url filled at spawn)
PROVIDER_CMDS: dict[str, list[str]] = {
    "grok": [PY, str(REPO / "grok_acp_worker.py"), "--timeout", "2400"],
    "hermes": [PY, str(REPO / "hermes_acp_worker.py")],
    "echo": [PY, str(REPO / "echo_worker.py")],
    "agy": [PY, str(REPO / "agy_worker.py")],
    "cursor": [PY, str(REPO / "cursor_worker.py")],
    "kilo": [PY, str(REPO / "kilo_worker.py")],
    "kilo-acp": [PY, str(REPO / "kilo_acp_worker.py")],
    "opencode": [PY, str(REPO / "opencode_worker.py")],
    "opencode-acp": [PY, str(REPO / "opencode_acp_worker.py")],
    # codex / claude: use echo as safe dogfood fallback until dedicated workers land
    "codex": [PY, str(REPO / "echo_worker.py")],
    "claude": [PY, str(REPO / "echo_worker.py")],
}


@dataclass
class Child:
    identity: str
    provider: str
    proc: asyncio.subprocess.Process
    model: str | None = None
    started_at: float = field(default_factory=time.time)
    ready: bool = False


class Supervisor:
    def __init__(
        self, nats_url: str, repo: Path, python: str,
        nats_auth: dict[str, Any] | None = None,
    ) -> None:
        self.nats_url = nats_url
        self.repo = repo
        self.python = python
        # Auth/TLS kwargs for the supervisor's own connection. When empty,
        # connect_nats resolves NATS_* env vars at connect time. Spawned
        # child workers inherit NATS_* env vars via create_subprocess_exec.
        self.nats_auth: dict[str, Any] = nats_auth or {}
        self.nc = NATSClient()
        self.children: dict[str, Child] = {}
        self._presence_waiters: dict[str, list[asyncio.Future[bool]]] = {}
        self._lock = asyncio.Lock()

    async def start(self) -> None:
        connect_kwargs: dict[str, Any] = {"name": "supervisor", **self.nats_auth}
        self.nc = await connect_nats(self.nats_url, **connect_kwargs)
        await self.nc.subscribe("hub.worker.ensure", cb=self._on_ensure)
        await self.nc.subscribe("hub.worker.stop", cb=self._on_stop)
        await self.nc.subscribe("hub.worker.list", cb=self._on_list)
        await self.nc.subscribe("hub.worker.models", cb=self._on_models)
        await self.nc.subscribe("hub.worker.providers", cb=self._on_providers)
        await self.nc.subscribe("hub.presence", cb=self._on_presence)
        print(f"[supervisor] ready on {self.nats_url} (repo={self.repo})")

    def _cmd(self, provider: str, identity: str, model: str | None = None) -> list[str]:
        base = PROVIDER_CMDS.get(provider)
        if not base:
            raise ValueError(f"unknown provider: {provider}")
        # rebind python path in case template used PY from import time
        argv = [self.python if p == PY or p.endswith("python3") else p for p in base]
        # ensure script path absolute under self.repo
        if len(argv) >= 2 and argv[1].endswith(".py"):
            argv[1] = str(self.repo / Path(argv[1]).name)
        argv.extend(
            [
                "--identity",
                identity,
                "--repo",
                str(self.repo),
                "--nats-url",
                self.nats_url,
            ]
        )
        if model:
            argv.extend(["--model", model])
        return argv

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
        waiters = self._presence_waiters.pop(identity, [])
        for fut in waiters:
            if not fut.done():
                fut.set_result(True)

    async def _wait_ready(self, identity: str, timeout: float = 25.0) -> bool:
        child = self.children.get(identity)
        if child and child.ready:
            return True
        loop = asyncio.get_running_loop()
        fut: asyncio.Future[bool] = loop.create_future()
        self._presence_waiters.setdefault(identity, []).append(fut)
        try:
            return await asyncio.wait_for(fut, timeout=timeout)
        except asyncio.TimeoutError:
            # process up is still useful even if presence delayed
            child = self.children.get(identity)
            return bool(child and child.proc.returncode is None)
        finally:
            lst = self._presence_waiters.get(identity) or []
            if fut in lst:
                lst.remove(fut)

    async def _spawn(self, identity: str, provider: str, model: str | None = None) -> dict[str, Any]:
        async with self._lock:
            existing = self.children.get(identity)
            if existing and existing.proc.returncode is None:
                # Restart if model changed so operators can swap models after launch.
                if model is not None and existing.model != model:
                    print(
                        f"[supervisor] model change for {identity}: "
                        f"{existing.model!r} → {model!r}; restarting"
                    )
                    # release lock path via unlock-free stop: stop outside would deadlock;
                    # stop the process here then fall through to spawn.
                    try:
                        os.killpg(existing.proc.pid, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                    except Exception:
                        try:
                            existing.proc.terminate()
                        except Exception:
                            pass
                    try:
                        await asyncio.wait_for(existing.proc.wait(), timeout=5)
                    except asyncio.TimeoutError:
                        try:
                            os.killpg(existing.proc.pid, signal.SIGKILL)
                        except Exception:
                            existing.proc.kill()
                    self.children.pop(identity, None)
                else:
                    return {
                        "ok": True,
                        "status": "already_running",
                        "identity": identity,
                        "provider": existing.provider,
                        "model": existing.model,
                        "pid": existing.proc.pid,
                        "ready": existing.ready,
                    }

            argv = self._cmd(provider, identity, model=model)
            print(f"[supervisor] spawn {identity} provider={provider}: {' '.join(argv)}")
            proc = await asyncio.create_subprocess_exec(
                *argv,
                cwd=str(self.repo),
                stdout=asyncio.subprocess.DEVNULL,
                stderr=asyncio.subprocess.DEVNULL,
                start_new_session=True,
            )
            self.children[identity] = Child(
                identity=identity, provider=provider, proc=proc, model=model, ready=False
            )

        ready = await self._wait_ready(identity, timeout=30.0)
        child = self.children.get(identity)
        if not child or child.proc.returncode is not None:
            return {
                "ok": False,
                "error": "worker exited during start",
                "identity": identity,
                "provider": provider,
            }
        return {
            "ok": True,
            "status": "started" if ready else "started_pending_presence",
            "identity": identity,
            "provider": provider,
            "model": model,
            "pid": child.proc.pid,
            "ready": ready,
        }

    async def _stop(self, identity: str) -> dict[str, Any]:
        async with self._lock:
            child = self.children.pop(identity, None)
        if not child:
            return {"ok": True, "status": "not_running", "identity": identity}
        try:
            os.killpg(child.proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        except Exception:
            try:
                child.proc.terminate()
            except Exception:
                pass
        try:
            await asyncio.wait_for(child.proc.wait(), timeout=5)
        except asyncio.TimeoutError:
            try:
                os.killpg(child.proc.pid, signal.SIGKILL)
            except Exception:
                child.proc.kill()
        return {"ok": True, "status": "stopped", "identity": identity, "pid": child.proc.pid}

    async def _reply(self, msg, body: dict[str, Any]) -> None:
        if msg.reply:
            await self.nc.publish(msg.reply, json.dumps(body).encode())

    async def _on_ensure(self, msg) -> None:
        try:
            req = json.loads(msg.data.decode() or "{}")
        except Exception as e:
            await self._reply(msg, {"ok": False, "error": f"bad json: {e}"})
            return
        identity = (req.get("identity") or "").strip()
        provider = (req.get("provider") or "").strip().lower()
        model = (req.get("model") or "").strip() or None
        if not identity or not provider:
            await self._reply(msg, {"ok": False, "error": "identity and provider required"})
            return
        if provider not in PROVIDER_CMDS:
            await self._reply(
                msg,
                {
                    "ok": False,
                    "error": f"unknown provider {provider}",
                    "known": sorted(PROVIDER_CMDS),
                },
            )
            return
        try:
            result = await self._spawn(identity, provider, model=model)
        except Exception as e:
            result = {"ok": False, "error": str(e), "identity": identity, "provider": provider}
        await self._reply(msg, result)
        print(f"[supervisor] ensure → {result}")

    async def _on_stop(self, msg) -> None:
        try:
            req = json.loads(msg.data.decode() or "{}")
        except Exception as e:
            await self._reply(msg, {"ok": False, "error": f"bad json: {e}"})
            return
        identity = (req.get("identity") or "").strip()
        if not identity:
            await self._reply(msg, {"ok": False, "error": "identity required"})
            return
        result = await self._stop(identity)
        await self._reply(msg, result)
        print(f"[supervisor] stop → {result}")

    async def _on_list(self, msg) -> None:
        # reap dead
        dead = [i for i, c in self.children.items() if c.proc.returncode is not None]
        for i in dead:
            self.children.pop(i, None)
        workers = [
            {
                "identity": c.identity,
                "provider": c.provider,
                "model": c.model,
                "pid": c.proc.pid,
                "ready": c.ready,
                "alive": c.proc.returncode is None,
            }
            for c in self.children.values()
        ]
        await self._reply(msg, {"ok": True, "workers": workers})

    async def _on_models(self, msg) -> None:
        """Live model list for any provider via model_catalog (CLI/config/static)."""
        try:
            req = json.loads(msg.data.decode() or "{}")
        except Exception as e:
            await self._reply(msg, {"ok": False, "error": f"bad json: {e}", "models": []})
            return
        provider = (req.get("provider") or "").strip().lower()
        refresh = bool(req.get("refresh"))
        if not provider:
            await self._reply(msg, {"ok": False, "error": "provider required", "models": []})
            return
        try:
            from worker_backends.model_catalog import clear_cache, list_models

            if refresh:
                clear_cache(provider)
            result = await list_models(provider, use_cache=not refresh)
        except Exception as e:
            result = {"ok": False, "provider": provider, "error": str(e), "models": []}
        await self._reply(msg, result)

    async def _on_providers(self, msg) -> None:
        """Catalog of model sources for all known/config providers."""
        try:
            from worker_backends.model_catalog import list_provider_catalog

            result = list_provider_catalog()
            # annotate which providers have spawn entrypoints
            for p in result.get("providers") or []:
                p["spawnable"] = p.get("id") in PROVIDER_CMDS
            result["spawnable"] = sorted(PROVIDER_CMDS)
        except Exception as e:
            result = {"ok": False, "error": str(e), "providers": []}
        await self._reply(msg, result)

    async def run_forever(self) -> None:
        await self.start()
        while True:
            await asyncio.sleep(2)
            # reap
            for ident, child in list(self.children.items()):
                if child.proc.returncode is not None:
                    print(f"[supervisor] reaped {ident} code={child.proc.returncode}")
                    self.children.pop(ident, None)


async def _amain(args: argparse.Namespace) -> None:
    # Build auth kwargs for the supervisor's own connection. Child workers
    # inherit NATS_* env vars via create_subprocess_exec, so no per-child
    # forwarding is needed here.
    nats_auth: dict[str, Any] = {}
    for key in (
        "token", "user", "password", "ca_file",
        "cert_file", "key_file", "credentials_file", "nkeys_seed",
    ):
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
    )
    await sup.run_forever()


def main() -> None:
    p = argparse.ArgumentParser(description="nats-hub on-demand worker supervisor")
    p.add_argument("--nats-url", default=os.environ.get("NATS_URL", "nats://127.0.0.1:4222"))
    p.add_argument("--repo", default=str(REPO))
    p.add_argument("--python", default=None, help="Python for child workers (default: this interpreter)")
    # Auth + TLS flags. NATS_* env vars are the defaults; flags win.
    # Spawned child workers inherit NATS_* env vars automatically.
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
    p.add_argument(
        "--tls-insecure",
        action="store_true",
        help="Disable cert verification. Refused unless NATS_ALLOW_INSECURE=1.",
    )
    args = p.parse_args()
    try:
        asyncio.run(_amain(args))
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
