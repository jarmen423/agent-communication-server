#!/usr/bin/env python3
"""Hosted-flow dogfood: one tenant, end to end, on this machine.

Run through the operator stack (it provides the operator/accounts/resolver
and `NATS_URL`; `--nats-only` so we start our own tenant hub-server):

    scripts/dev/operator_stack.sh --nats-only .venv/bin/python scripts/dogfood_hosted.py [--real-worker claude]

What it does, printing PASS/FAIL per step with real output:

 1. tenant T_A hub-server: --durable --require-bound-identity, admin `me`,
    --backup-dir (operator-mode creds)
 2. DEV-ONLY stub control plane; `acs login --headless` (scripted approve)
 3. `acs agent add` me / echo-dev / lock-dev (+ the real worker, if asked)
 4. workers via `acs worker run` (echo; optionally a real CLI agent)
 5. `acs mcp install claude` into a temp HOME; start the stdio MCP server
    exactly as the generated config says
 6. through MCP: whoami, delegate to echo, delegate to the real worker,
    a two-task wave with a dependency, a file lock contended by another
    agent (hub-lock CLI) and released
 7. `hub-server backup` by the admin → stop hub → `hub-server restore`

The remote MCP endpoint (web chat apps) and the visualizer have their own
end-to-end runs: `make test-remote-mcp`, `tests/e2e_visualizer/run_smoke.sh`.
`scripts/dogfood_hosted.sh` runs all three.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import httpx
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

REPO = Path(__file__).resolve().parents[1]
BIN = REPO / "target" / "debug"
ACS = BIN / "acs"
FAILS: list[str] = []


def step(msg: str) -> None:
    print(f"\n── {msg}", flush=True)


def ok(cond: bool, msg: str, detail: str = "") -> None:
    print(f"{'PASS' if cond else 'FAIL'}  {msg}" + (f"  — {detail}" if detail else ""), flush=True)
    if not cond:
        FAILS.append(msg)


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Stack:
    def __init__(self, tmp: Path) -> None:
        self.tmp = tmp
        self.procs: list[subprocess.Popen] = []
        self.op = Path(os.environ["OPERATOR_DIR"])
        self.nats_url = os.environ["NATS_URL"]
        self.cp_port = free_port()
        self.server = f"http://127.0.0.1:{self.cp_port}"
        self.acs_home = tmp / "acs"
        self.home = tmp / "client-home"
        self.backups = tmp / "backups"
        self.backups.mkdir()
        self.db = tmp / "db-T_A"
        self.env = {**{k: v for k, v in os.environ.items() if not k.startswith("NATS_")},
                    "NATS_URL": self.nats_url, "ACS_HOME": str(self.acs_home),
                    "ACS_NO_KEYRING": "1", "ACS_SERVER": self.server,
                    "ACS_WORKER_DIR": str(REPO), "PYTHONUNBUFFERED": "1"}
        self.hub: subprocess.Popen | None = None

    def spawn(self, name: str, args: list[str], extra: dict | None = None) -> subprocess.Popen:
        log = (self.tmp / f"{name}.log").open("w")
        p = subprocess.Popen(args, env={**self.env, **(extra or {})}, cwd=REPO,
                             stdout=log, stderr=subprocess.STDOUT, text=True)
        self.procs.append(p)
        return p

    def acs(self, *args: str, check: bool = True) -> subprocess.CompletedProcess:
        r = subprocess.run([str(ACS), *args], env=self.env, cwd=REPO, text=True,
                           capture_output=True, timeout=60)
        if check and r.returncode != 0:
            raise RuntimeError(f"acs {' '.join(args)}: {r.stderr.strip()}")
        return r

    def creds(self, identity: str) -> str:
        return str(self.acs_home / "creds" / f"{identity}.creds")

    def start_hub(self, db: Path) -> subprocess.Popen:
        self.hub = self.spawn(f"hub-{db.name}", [
            str(BIN / "hub-server"), "--nats-url", self.nats_url, "--db-path", str(db),
            "--durable", "--require-bound-identity", "--api-admin", "me",
            "--backup-dir", str(self.backups)],
            {"NATS_CREDENTIALS_FILE": str(self.op / "creds/T_A/hub-server.creds")})
        time.sleep(2.5)
        return self.hub

    def stop(self, p: subprocess.Popen) -> None:
        if p.poll() is None:
            p.terminate()
            try:
                p.wait(timeout=10)
            except subprocess.TimeoutExpired:
                p.kill()

    def close(self) -> None:
        for p in reversed(self.procs):
            self.stop(p)


def headless_login(st: Stack) -> str:
    proc = subprocess.Popen([str(ACS), "login", "--headless"], env=st.env, cwd=REPO, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    line = proc.stdout.readline()
    url = re.search(r"https?://\S+", line).group()
    httpx.get(url, timeout=10)  # DEV stub: visiting the page approves the dev user
    out, err = proc.communicate(timeout=20)
    if proc.returncode != 0:
        raise RuntimeError(f"acs login: {err.strip()}")
    return (line + out).strip()


async def call(session: ClientSession, tool: str, args: dict) -> dict:
    res = await session.call_tool(tool, args)
    text = res.content[0].text if res.content else "{}"
    try:
        data = json.loads(text)
    except ValueError:
        data = {"ok": False, "error": text}
    if res.isError:
        data.setdefault("ok", False)
    return data


async def mcp_journey(st: Stack, real: str | None) -> None:
    config = json.loads((st.home / ".claude.json").read_text())["mcpServers"]["nats-hub"]
    params = StdioServerParameters(command=config["command"], args=config["args"],
                                   env={**st.env, **config["env"]})
    async with stdio_client(params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            tools = {t.name for t in (await session.list_tools()).tools}
            ok({"delegate_task", "create_wave", "lock_files"} <= tools,
               "MCP server started from the acs-generated config", f"{len(tools)} tools")

            who = await call(session, "whoami", {})
            ok(who.get("data", {}).get("identity") == "me" or "me" in json.dumps(who),
               "whoami → identity from the acs credential", json.dumps(who.get("data", who))[:120])

            step("delegate through MCP")
            r = await call(session, "delegate_task",
                           {"to": "echo-dev", "prompt": "hello hosted", "timeout": 30})
            got = r.get("data", {}).get("reply", {}).get("result")
            ok(got == "echo: detsoh olleh", "delegate_task → echo-dev", repr(got))

            if real:
                r = await call(session, "delegate_task", {
                    "to": f"{real}-dev", "timeout": 240,
                    "prompt": "Reply with exactly the single word: pong. No other text."})
                reply = r.get("data", {}).get("reply", {})
                result = str(reply.get("result", "")).strip()
                ok(reply.get("status") == "done" and "pong" in result.lower(),
                   f"delegate_task → real {real} worker", repr(result[:200]) or json.dumps(r)[:200])

            step("wave: two tasks, the second depends on the first")
            w = await call(session, "create_wave", {"goal": "dogfood wave", "tasks": [
                {"task_id": "a", "worker": "echo-dev", "goal": "first task", "write_scope": ["docs/a/"]},
                {"task_id": "b", "worker": "echo-dev", "goal": "second task",
                 "write_scope": ["docs/b/"], "dependencies": ["a"]},
            ]})
            wave_id = (w.get("data") or {}).get("wave_id") or (w.get("data") or {}).get("wave", {}).get("wave_id")
            ok(bool(w.get("ok")) and bool(wave_id), "create_wave", str(wave_id))
            if wave_id:
                s = await call(session, "spawn_wave", {"wave_id": wave_id, "timeout": 120})
                ok(bool(s.get("ok")), "spawn_wave", json.dumps(s.get("data", s))[:160])
                status = None
                for _ in range(90):
                    st_ = await call(session, "wave_status", {"wave_id": wave_id})
                    data = st_.get("data") or {}
                    status = (data.get("wave") or data).get("status")
                    if status in ("done", "completed", "failed", "cancelled"):
                        break
                    await asyncio.sleep(1)
                ok(status in ("done", "completed"), "wave ran both tasks to completion", f"status={status}")

            step("file lock contended by another agent")
            lock_env = {**st.env, "NATS_CREDENTIALS_FILE": st.creds("lock-dev"),
                        "NATS_HUB_IDENTITY": "lock-dev"}
            acq = subprocess.run([str(BIN / "hub-lock"), "--nats-url", st.nats_url, "acquire",
                                  "--scope", "dogfood-repo", "--path", "src/main.rs",
                                  "--reason", "refactor"],
                                 env=lock_env, text=True, capture_output=True, timeout=30)
            ok(acq.returncode == 0 and "ACQUIRED" in acq.stdout, "lock-dev acquires src/main.rs (hub-lock)",
               acq.stdout.strip().splitlines()[0] if acq.stdout else acq.stderr.strip()[:160])
            lock_id = next(iter(re.findall(r"\b[0-9a-f]{8}-[0-9a-f-]{27}\b", acq.stdout)), None)
            mine = await call(session, "lock_files", {"scope": "dogfood-repo", "paths": ["./src/main.rs"]})
            ok(not mine.get("ok"), "me: lock_files on the same path is refused (all-or-nothing)",
               mine.get("error", "")[:80])
            waiter = asyncio.create_task(call(session, "wait_for_files",
                                              {"scope": "dogfood-repo", "paths": ["src/main.rs"], "timeout": 30}))
            await asyncio.sleep(1.5)
            ok(not waiter.done(), "me: wait_for_files blocks while lock-dev holds it")
            t0 = time.monotonic()
            rel = subprocess.run([str(BIN / "hub-lock"), "--nats-url", st.nats_url, "release", "--lock-id", lock_id or "?"],
                                 env=lock_env, text=True, capture_output=True, timeout=30)
            ok(rel.returncode == 0, "lock-dev releases", rel.stdout.strip()[:120] or rel.stderr.strip()[:120])
            freed = await asyncio.wait_for(waiter, 35)
            ok(bool(freed.get("ok")) and freed["data"].get("free"), "me: wait_for_files returns free",
               f"{time.monotonic() - t0:.1f}s after release")
            mine = await call(session, "lock_files", {"scope": "dogfood-repo", "paths": ["src/main.rs"]})
            ok(bool(mine.get("ok")), "me: lock_files now granted")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--real-worker", choices=["claude", "codex", "hermes", "grok"],
                    help="also delegate a tiny prompt to this real CLI agent (costs a few tokens)")
    args = ap.parse_args()
    if "OPERATOR_DIR" not in os.environ:
        sys.exit("run via: scripts/dev/operator_stack.sh --nats-only .venv/bin/python scripts/dogfood_hosted.py")
    for b in ("acs", "hub-server", "hub-lock", "hub-worker"):
        if not (BIN / b).is_file():
            sys.exit(f"missing {BIN / b}; run make build")

    tmp = Path(tempfile.mkdtemp(prefix="dogfood-hosted-"))
    st = Stack(tmp)
    try:
        step("tenant T_A hub-server (operator mode, durable, bound identity, admin me, --backup-dir)")
        hub = st.start_hub(st.db)
        ok(hub.poll() is None, "hub-server running", f"db={st.db}")

        step("DEV-ONLY stub control plane + acs login")
        cp = st.spawn("stub", [sys.executable, "-m", "stub_control_plane", "--port", str(st.cp_port),
                               "--resource", st.server + "/mcp", "--nsc-home", str(st.op / "nsc"),
                               "--tenant", "T_A", "--service-key-file", str(tmp / "admin.key"),
                               "--nats-url", st.nats_url])
        for _ in range(100):
            try:
                if httpx.get(st.server + "/healthz", timeout=1).status_code == 200:
                    break
            except httpx.HTTPError:
                time.sleep(0.1)
        ok(cp.poll() is None, "stub control plane up", st.server)
        login = headless_login(st)
        ok("Logged in" in login, "acs login --headless", login.splitlines()[-1])

        step("acs agent add")
        roles = {"me": "orchestrator", "echo-dev": "worker", "lock-dev": "worker"}
        if args.real_worker:
            roles[f"{args.real_worker}-dev"] = "worker"
        for ident, role in roles.items():
            r = st.acs("agent", "add", ident, "--role", role)
            ok(Path(st.creds(ident)).is_file(), f"acs agent add {ident} ({role})", r.stdout.strip()[:100])
        mode = oct(Path(st.creds("me")).stat().st_mode & 0o777)
        ok(mode == "0o600", "creds file is 0600", mode)

        step("workers via acs worker run")
        st.spawn("echo-dev", [str(ACS), "worker", "run", "--identity", "echo-dev", "echo"])
        if args.real_worker:
            extra = {"claude": ["--repo", str(tmp), "--model", "haiku", "--permission-mode", "plan"]}
            st.spawn(f"{args.real_worker}-dev", [str(ACS), "worker", "run", "--identity",
                                                 f"{args.real_worker}-dev", args.real_worker,
                                                 "--", *extra.get(args.real_worker, [])])
        time.sleep(3)

        step("acs mcp install claude (temp HOME)")
        r = st.acs("mcp", "install", "claude", "--identity", "me", "--home", str(st.home))
        cfg = json.loads((st.home / ".claude.json").read_text())["mcpServers"]["nats-hub"]
        ok(set(cfg["env"]) == {"NATS_URL", "NATS_CREDENTIALS_FILE", "NATS_HUB_IDENTITY"},
           "MCP config written with creds, no secrets inline", r.stdout.strip()[:100])

        asyncio.run(mcp_journey(st, args.real_worker))

        step("backup (admin `me`) → stop hub → restore into a fresh path")
        b = subprocess.run([str(BIN / "hub-server"), "backup", "--nats-url", st.nats_url,
                            "--identity", "me", "--name", "dogfood.surql"],
                           env={**st.env, "NATS_CREDENTIALS_FILE": st.creds("me")},
                           text=True, capture_output=True, timeout=60)
        ok(b.returncode == 0 and (st.backups / "dogfood.surql").is_file(), "hub-server backup",
           b.stdout.strip() or b.stderr.strip()[:200])
        st.stop(hub)
        r = subprocess.run([str(BIN / "hub-server"), "restore", "--db-path", str(tmp / "restored"),
                            "--from", str(st.backups / "dogfood.surql")],
                           text=True, capture_output=True, timeout=120)
        counts = dict(re.findall(r"^\s*(\w+)\s+(\d+)", r.stdout, re.M))
        ok(r.returncode == 0 and int(counts.get("envelopes", 0)) > 0 and int(counts.get("waves", 0)) >= 1,
           "hub-server restore", " ".join(f"{k}={v}" for k, v in counts.items()))
    except Exception as e:  # noqa: BLE001 - report and fail
        ok(False, f"unexpected error: {type(e).__name__}: {e}")
    finally:
        try:
            st.acs("logout", check=False)
        except Exception:  # noqa: BLE001
            pass
        st.close()
        if FAILS:
            print(f"\nlogs kept in {tmp}", flush=True)
        else:
            shutil.rmtree(tmp, ignore_errors=True)

    print("\n" + ("=== hosted dogfood: ALL PASS ===" if not FAILS
                  else f"=== hosted dogfood: {len(FAILS)} FAILED: {FAILS} ==="), flush=True)
    return 1 if FAILS else 0


if __name__ == "__main__":
    sys.exit(main())
