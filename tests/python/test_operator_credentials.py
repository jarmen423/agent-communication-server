"""Real expiry/reload/revocation with HubClient, a Python worker and MCP.

NSC's shortest relative expiry is one minute. All three clients run across
that same expiry, so the test takes about 65 seconds in the operator CI step.
"""
import asyncio
import base64
import json
import os
import signal
import sys
import time
from pathlib import Path

import nats
import pytest

from test_operator_isolation import api, connect, creds, envelope

pytestmark = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_OPERATOR_TEST_STACK"),
    reason="requires scripts/dev/operator_stack.sh (pinned nsc + two tenant hubs)",
)
REPO = Path(__file__).resolve().parents[2]


async def nsc(*args):
    process = await asyncio.create_subprocess_exec(
        os.environ["NSC_BIN"], "-H", str(Path(os.environ["OPERATOR_DIR"]) / "nsc"), *args,
        stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.STDOUT)
    output, _ = await asyncio.wait_for(process.communicate(), timeout=15)
    assert process.returncode == 0, output.decode()
    return output


async def renew(user, expiry):
    role = "orchestrator" if user == "rotate-mcp" else "worker"
    await nsc("edit", "user", "--account", "T_A", "--name", user, "-K", role, "--expiry", expiry)
    path = Path(creds("T_A", user))
    await nsc("generate", "creds", "--account", "T_A", "--name", user,
              "--output-file", str(path.with_suffix(".new")))
    path.with_suffix(".new").replace(path)  # atomic, never truncate the live file
    jwt = path.read_text().split("-----BEGIN NATS USER JWT-----\n")[1].splitlines()[0]
    payload = jwt.split(".")[1]
    return json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))


async def send_worker_task(bob, marker):
    channel = f"task.rotation{marker}"
    sub = await bob.subscribe(f"channel.{channel}")
    try:
        await bob.flush()
        msg = json.loads(envelope("bob", channel, marker))
        msg["meta"]["to"] = "rotate-python"
        msg["payload"] = {"prompt": marker, "task_channel": channel}
        await bob.publish(f"hub.pub.bob.{channel}", json.dumps(msg).encode())
        for _ in range(10):
            result = json.loads((await sub.next_msg(timeout=10)).data)
            if result["meta"]["kind"] == "message" and result["payload"].get("status") == "done":
                assert result["payload"]["result"] == f"echo: {marker[::-1]}"
                return
        pytest.fail("worker did not send a terminal result")
    finally:
        await sub.unsubscribe()


def test_clients_reload_after_real_jwt_expiry_and_user_revocation(monkeypatch):
    from mcp_server import hub_connection as conn
    monkeypatch.setenv("NATS_HUB_IDENTITY", "rotate-mcp")
    monkeypatch.setenv("NATS_CREDENTIALS_FILE", creds("T_A", "rotate-mcp"))
    monkeypatch.setattr(conn, "NATS_URL", os.environ["NATS_URL"])
    monkeypatch.setattr(conn, "_nc", None)
    monkeypatch.setattr(conn, "_connect_lock", asyncio.Lock())

    async def run():
        processes = []
        log = None
        bob = None
        mcp = None
        try:
            claims = {u: await renew(u, "1m") for u in ("rotate-rust", "rotate-python", "rotate-mcp")}
            old = Path(os.environ["OPERATOR_DIR"]) / "expired.creds"
            old.write_bytes(Path(creds("T_A", "rotate-mcp")).read_bytes())
            bob = await connect("T_A", "bob")
            channel = "task.rotation-rust"
            rust = await asyncio.create_subprocess_exec(
                str(REPO / "target/debug/hub-observe"), "--identity", "rotate-rust", "--json",
                "--channel", channel, "--nats-url", os.environ["NATS_URL"],
                env={**os.environ, "NATS_CREDENTIALS_FILE": creds("T_A", "rotate-rust"),
                     "NATS_HUB_IDENTITY": "rotate-rust", "RUST_LOG": "off"},
                stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
                start_new_session=True)
            processes.append(rust)
            assert b"listening" in await asyncio.wait_for(rust.stderr.readline(), timeout=10)
            log = open(Path(os.environ["OPERATOR_DIR"]) / "rotation-worker.log", "wb")
            worker = await asyncio.create_subprocess_exec(
                sys.executable, str(REPO / "echo_worker.py"), "--identity", "rotate-python",
                "--nats-url", os.environ["NATS_URL"],
                env={**os.environ, "NATS_CREDENTIALS_FILE": creds("T_A", "rotate-python"),
                     "NATS_HUB_IDENTITY": "rotate-python", "PYTHONUNBUFFERED": "1"},
                stdout=log, stderr=log, start_new_session=True)
            processes.append(worker)
            mcp = await conn.get_nc()
            await api(mcp, "rotate-mcp", "ping")
            await send_worker_task(bob, "before")

            async def rust_receives(marker):
                await bob.publish(f"hub.pub.bob.{channel}", envelope("bob", channel, marker))
                line = await asyncio.wait_for(rust.stdout.readline(), timeout=10)
                assert json.loads(line)["payload"]["marker"] == marker, line
            await rust_receives("before")
            # Extend all JWTs on disk while leaving connections/processes intact.
            for user in claims:
                fresh = await renew(user, "1h")
                assert fresh["sub"] == claims[user]["sub"], "refresh must retain the user's nkey"
                assert fresh["exp"] > claims[user]["exp"]
            deadline = max(c["exp"] for c in claims.values()) + 3
            print("Waiting for actual 60-second JWT expiry (all clients concurrently)", flush=True)
            await asyncio.sleep(max(0, deadline - time.time()))
            assert await conn.get_nc() is mcp, "MCP must retain its connection and subscriptions"
            await api(mcp, "rotate-mcp", "ping")
            assert mcp.stats["reconnects"] >= 1
            await rust_receives("after")
            await send_worker_task(bob, "after")
            assert rust.returncode is None and worker.returncode is None
            print("PASS real JWT expiry: same Rust HubClient, Python worker and MCP reconnected with renewed files")
            async def ignore_error(error):
                pass
            with pytest.raises(nats.errors.Error):
                await nats.connect(os.environ["NATS_URL"], user_credentials=str(old),
                                   allow_reconnect=False, connect_timeout=1, error_cb=ignore_error)
            print("PASS old unrefreshed JWT rejected after expiry")

            # Watch an existing connection, revoke its user, push the account JWT
            # through the resolver, and verify both eviction and a new-login denial.
            revoked = await connect("T_A", "rotate-python", allow_reconnect=False, error_cb=ignore_error)
            try:
                await nsc("revocations", "add-user", "--account", "T_A", "--name", "rotate-python", "--at", "0")
                await nsc("push", "--account", "T_A", "--account-jwt-server-url", os.environ["NATS_URL"])
                for _ in range(50):
                    if revoked.is_closed:
                        break
                    await asyncio.sleep(0.1)
                assert revoked.is_closed, "revocation did not disconnect an existing user"
                with pytest.raises(nats.errors.Error):
                    await connect("T_A", "rotate-python", allow_reconnect=False,
                                  connect_timeout=1, error_cb=ignore_error)
                print("PASS account revocation: connected user disconnected, fresh connection rejected")
            finally:
                await revoked.close()
        finally:
            for process in processes:
                if process.returncode is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        await asyncio.wait_for(process.wait(), timeout=5)
                    except asyncio.TimeoutError:
                        os.killpg(process.pid, signal.SIGKILL)
                        await process.wait()
            if log:
                log.close()
            if bob:
                await bob.close()
            if mcp:
                await mcp.close()
    asyncio.run(run())
