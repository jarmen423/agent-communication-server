"""End to end: web-chat connector → remote MCP endpoint ×2 → durable hub → worker.

Runs on the operator stack (scripts/dev/operator_stack.sh): tenant T_A's
durable, bound hub-server, an echo worker with its own creds, the stub
control plane (OAuth AS + NATS credential issuer) and two remote MCP
endpoint replicas behind a TCP proxy that the test pins to one replica or
the other. An MCP SDK client does the full OAuth dance (DCR → authorize →
consent → token), then every "chat turn" is a separate HTTP session.
"""
import asyncio
import json
import os
import socket
import subprocess
import sys
import time
from pathlib import Path
from urllib.parse import parse_qs, urlparse

import httpx
import pytest

pytestmark = pytest.mark.skipif(
    not os.environ.get("NATS_HUB_OPERATOR_TEST_STACK"),
    reason="requires scripts/dev/operator_stack.sh (make setup/build + install_nsc.sh)",
)

REPO = Path(__file__).resolve().parents[2]
PY = sys.executable
CALLBACK = "http://127.0.0.1:33418/callback"  # never served: the test reads the redirect


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class PinnedProxy:
    """TCP proxy standing in for a load balancer; ``target`` picks the
    replica for each new connection."""

    def __init__(self, backends: list[int]) -> None:
        self.backends, self.target, self.port = backends, 0, free_port()
        self.hits = [0] * len(backends)

    async def start(self) -> None:
        self.server = await asyncio.start_server(self._handle, "127.0.0.1", self.port)

    async def _handle(self, reader, writer) -> None:
        idx = self.target
        self.hits[idx] += 1
        up_r, up_w = await asyncio.open_connection("127.0.0.1", self.backends[idx])

        async def pipe(r, w):
            try:
                while data := await r.read(65536):
                    w.write(data)
                    await w.drain()
            except (ConnectionError, asyncio.CancelledError):
                pass
            finally:
                w.close()

        await asyncio.gather(pipe(reader, up_w), pipe(up_r, writer))

    async def stop(self) -> None:
        self.server.close()


def wait_http(url: str, procs: list, timeout: float = 20.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        for p in procs:
            assert p.poll() is None, f"process exited: {p.args}"
        try:
            if httpx.get(url, timeout=1.0).status_code == 200:
                return
        except httpx.HTTPError:
            pass
        time.sleep(0.2)
    raise TimeoutError(url)


@pytest.fixture(scope="module")
def stack(tmp_path_factory):
    tmp = tmp_path_factory.mktemp("remote-mcp")
    op = Path(os.environ["OPERATOR_DIR"])
    cp_port, ep_ports = free_port(), [free_port(), free_port()]
    proxy = PinnedProxy(ep_ports)
    issuer = f"http://127.0.0.1:{cp_port}"
    resource = f"http://127.0.0.1:{proxy.port}/mcp"  # the "load balancer" URL
    key_file = tmp / "service.key"
    env = {k: v for k, v in os.environ.items() if not k.startswith("NATS_HUB_IDENTITY")}
    logs, procs = [], []

    def spawn(name, args, extra_env=None):
        log = open(tmp / f"{name}.log", "w")
        logs.append(log)
        p = subprocess.Popen(args, cwd=REPO, env={**env, **(extra_env or {})},
                             stdout=log, stderr=subprocess.STDOUT)
        procs.append(p)
        return p

    def stop():
        for p in procs:
            p.terminate()
        for p in procs:
            try:
                p.wait(timeout=10)
            except subprocess.TimeoutExpired:
                p.kill()
        for log in logs:
            log.close()

    try:
        spawn("stub-cp", [PY, "-m", "stub_control_plane", "--port", str(cp_port),
                          "--resource", resource, "--nsc-home", str(op / "nsc"),
                          "--tenant", "T_A", "--service-key-file", str(key_file),
                          "--allow-loopback-redirects", "--access-ttl", "600"])
        wait_http(f"{issuer}/healthz", procs)
        for i, port in enumerate(ep_ports):
            spawn(f"ep{i}", [PY, "-m", "mcp_remote", "--port", str(port),
                             "--resource-url", resource, "--issuer", issuer,
                             "--nats-url", os.environ["NATS_URL"],
                             "--service-key-file", str(key_file)])
        spawn("echo-1", [PY, "echo_worker.py", "--identity", "echo-1",
                         "--nats-url", os.environ["NATS_URL"]],
              {"NATS_CREDENTIALS_FILE": str(op / "creds" / "T_A" / "echo-1.creds")})
        for port in ep_ports:
            wait_http(f"http://127.0.0.1:{port}/healthz", procs)
        time.sleep(1.0)  # echo worker attaches its inbox
        yield {"proxy": proxy, "issuer": issuer, "resource": resource, "tmp": tmp,
               "creds": op / "creds" / "T_A"}
    finally:
        stop()
        if os.environ.get("KEEP_LOGS"):
            print(f"remote MCP logs: {tmp}")


class MemoryStorage:
    def __init__(self):
        self.tokens = self.client = None

    async def get_tokens(self):
        return self.tokens

    async def set_tokens(self, tokens):
        self.tokens = tokens

    async def get_client_info(self):
        return self.client

    async def set_client_info(self, info):
        self.client = info


def oauth_provider(stack, storage):
    from mcp.client.auth import OAuthClientProvider
    from mcp.shared.auth import OAuthClientMetadata

    redirect = {}

    async def on_redirect(url: str) -> None:
        # The "browser": follow the authorization server's redirects until it
        # sends us back to the client callback with the code.
        async with httpx.AsyncClient() as http:
            nxt = url
            for _ in range(5):
                resp = await http.get(nxt)
                assert resp.status_code in (302, 303), resp.text
                nxt = resp.headers["location"]
                if nxt.startswith(CALLBACK):
                    redirect["url"] = nxt
                    return
        raise AssertionError("no callback redirect")

    async def on_callback():
        q = parse_qs(urlparse(redirect["url"]).query)
        return q["code"][0], q.get("state", [None])[0]

    meta = OAuthClientMetadata(client_name="e2e chat app", redirect_uris=[CALLBACK],
                               grant_types=["authorization_code", "refresh_token"],
                               response_types=["code"], scope="acs:hub",
                               token_endpoint_auth_method="none")
    return OAuthClientProvider(stack["resource"], meta, storage, on_redirect, on_callback)


async def turn(stack, auth, replica: int, calls):
    """One chat turn: a fresh HTTP connection + MCP session pinned to a replica."""
    from mcp import ClientSession
    from mcp.client.streamable_http import streamable_http_client
    from mcp.shared._httpx_utils import create_mcp_http_client

    stack["proxy"].target = replica
    async with create_mcp_http_client(auth=auth) as http, \
            streamable_http_client(stack["resource"], http_client=http) as (r, w, _):
        async with ClientSession(r, w) as s:
            await s.initialize()
            out = []
            for name, args in calls:
                res = await s.call_tool(name, args)
                text = res.content[0].text
                out.append((res.isError, json.loads(text) if text.startswith("{") else text))
            return out


def test_oauth_dance_and_turns_across_replicas(stack):
    import nats

    async def run():
        proxy = stack["proxy"]
        await proxy.start()
        storage = MemoryStorage()
        auth = oauth_provider(stack, storage)
        bob = await nats.connect(os.environ["NATS_URL"],
                                 user_credentials=str(stack["creds"] / "bob.creds"),
                                 inbox_prefix=b"_INBOX.bob")
        try:
            # Turn 1 on replica 0: the OAuth dance happens on the first call.
            [(err, who)] = await turn(stack, auth, 0, [("whoami", {})])
            assert not err, who
            ident = who["data"]["identity"]
            assert ident.startswith("mcp-web-") and who["data"]["tenant"] == "T_A"
            assert storage.tokens.refresh_token and storage.client.client_id
            print(f"PASS OAuth DCR→authorize→consent→token; identity {ident}")

            [(err, d)] = await turn(stack, auth, 0, [("delegate", {"to": "echo-1",
                                                                    "prompt": "hello hub"})])
            assert not err, d
            task_id, worker = d["data"]["task_id"], d["data"]["worker"]

            # Turn 3 on the other replica, a separate HTTP session.
            [(err, r)] = await turn(stack, auth, 1, [("get_task_result", {
                "task_id": task_id, "worker": worker, "wait_secs": 20})])
            assert not err, r
            assert r["data"]["state"] == "done", r
            assert r["data"]["result"] == "echo: " + "hello hub"[::-1]
            # Same answer from replica 0, with no wait.
            [(err, r0)] = await turn(stack, auth, 0, [("get_task_result", {
                "task_id": task_id, "worker": worker, "wait_secs": 0})])
            assert r0["data"]["result"] == r["data"]["result"]
            print("PASS delegate on replica 0 → result on replica 1 and 0")

            # A forged result from someone else on the task channel is ignored (C5).
            [(err, d)] = await turn(stack, auth, 1, [("get_task_result", {
                "task_id": task_id, "worker": "carol", "wait_secs": 0})])
            assert d["data"]["state"] == "running"

            # Identity comes from the token: a `from` argument can't change it.
            [(err, bad)] = await turn(stack, auth, 1, [("send_direct", {
                "to": "bob", "message": "x", "from": "bob"})])
            assert err and "authenticated identity" in json.dumps(bad)
            sub = await bob.subscribe("channel.inbox.bob")
            await bob.flush()
            [(err, sent)] = await turn(stack, auth, 1, [("send_direct", {
                "to": "bob", "message": "hi bob"})])
            assert not err, sent
            got = json.loads((await sub.next_msg(timeout=5)).data)
            assert got["meta"]["from"] == ident and got["payload"]["message"] == "hi bob"
            print("PASS identity stamped from the token, `from` argument refused")

            # Inbox: DMs sent while no turn runs; the read position is shared.
            for n in (1, 2):
                env = {"meta": {"id": f"dm-{n}-{time.time_ns()}", "from": "bob", "to": ident,
                                "channel": "dm", "kind": "message", "reply_to": None,
                                "timestamp": "2026-10-21T00:00:00Z"}, "payload": {"n": n}}
                await bob.publish(f"hub.pub.bob.dm", json.dumps(env).encode())
                await bob.flush()
                await asyncio.sleep(0.5)
                [(err, inbox)] = await turn(stack, auth, n % 2, [("read_inbox", {"wait_secs": 5})])
                assert not err, inbox
                assert [m["payload"] for m in inbox["data"]["messages"]] == [{"n": n}], inbox
            print("PASS inbox read on replica 1 then 0, each DM once")
        finally:
            await bob.drain()
            await proxy.stop()
        assert all(proxy.hits), proxy.hits

    asyncio.run(run())


def test_long_result_is_paged_and_refresh_rotates(stack):
    async def run():
        proxy = stack["proxy"]
        await proxy.start()
        storage = MemoryStorage()
        auth = oauth_provider(stack, storage)
        try:
            prompt = "".join(chr(0x61 + i % 26) for i in range(150_000)) + '"\\\n'
            [(err, d)] = await turn(stack, auth, 0, [("delegate", {"to": "echo-1",
                                                                    "prompt": prompt})])
            assert not err, d
            args = {"task_id": d["data"]["task_id"], "worker": "echo-1"}
            pages, cursor, replica = [], None, 1
            while True:
                call = {**args, "wait_secs": 30, **({"cursor": cursor} if cursor else {})}
                [(err, r)] = await turn(stack, auth, replica, [("get_task_result", call)])
                assert not err, r
                page = r["data"]
                assert len(json.dumps(page["result"], ensure_ascii=False)) - 2 <= 100_000
                pages.append(page["result"])
                cursor, replica = page["result_page"]["next_cursor"], 1 - replica
                if cursor is None:
                    break
            assert len(pages) >= 2 and "".join(pages) == "echo: " + prompt[::-1]
            print(f"PASS {len(pages)} pages, each ≤ 100k escaped chars, across replicas")

            # Refresh: a new access + refresh token; the old refresh token is spent.
            meta = (await httpx.AsyncClient().get(
                f"{stack['issuer']}/.well-known/oauth-authorization-server")).json()
            old = storage.tokens.refresh_token
            form = {"grant_type": "refresh_token", "refresh_token": old,
                    "client_id": storage.client.client_id}
            async with httpx.AsyncClient() as http:
                new = (await http.post(meta["token_endpoint"], data=form)).json()
                assert new["refresh_token"] != old and new["access_token"]
                again = await http.post(meta["token_endpoint"], data=form)
                assert again.status_code == 400, again.text
                # Reuse revoked the family: the rotated token is dead too.
                form["refresh_token"] = new["refresh_token"]
                assert (await http.post(meta["token_endpoint"], data=form)).status_code == 400
            print("PASS refresh rotation + reuse detection")
        finally:
            await proxy.stop()

    asyncio.run(run())


def test_sessions_and_waves_across_replicas(stack):
    async def run():
        proxy = stack["proxy"]
        await proxy.start()
        auth = oauth_provider(stack, MemoryStorage())
        try:
            [(err, s)] = await turn(stack, auth, 0, [("start_session", {
                "worker": "echo-1", "prompt": "first turn"})])
            assert not err, s
            sid = s["data"]["session_id"]
            replies, since = [], 0
            for replica in (1, 0, 1, 0, 1, 0):
                [(err, r)] = await turn(stack, auth, replica, [("session_replies", {
                    "session_id": sid, "since_seq": since, "wait_secs": 5})])
                assert not err, r
                replies += r["data"]["messages"]
                since = r["data"]["next_seq"]
                if any(m["from"] == "echo-1" and m["kind"] == "message" for m in replies):
                    break
            texts = json.dumps([m["payload"] for m in replies if m["from"] == "echo-1"])
            assert "first turn"[::-1] in texts, replies
            [(err, c)] = await turn(stack, auth, 1, [("close_session", {"session_id": sid})])
            assert not err, c
            print("PASS session started on replica 0, replies paged on both")

            [(err, w)] = await turn(stack, auth, 1, [("create_wave", {
                "goal": "remote wave", "tasks": [{"task_id": "t1", "worker": "echo-1",
                                                  "goal": "wave hello",
                                                  "write_scope": ["docs/x"]}]})])
            assert not err, w
            wave_id = w["data"]["wave_id"]
            [(err, sp)] = await turn(stack, auth, 0, [("spawn_wave", {"wave_id": wave_id})])
            assert not err, sp
            status = None
            for i in range(30):
                [(err, st)] = await turn(stack, auth, i % 2, [("wave_status", {"wave_id": wave_id})])
                assert not err, st
                status = st["data"]["wave"]["status"]
                if status not in ("pending", "running"):
                    break
                await asyncio.sleep(1)
            print(f"wave {wave_id} status: {status}")
            assert status == "completed", st
            print("PASS wave created on replica 1, spawned on 0, completed")
        finally:
            await proxy.stop()

    asyncio.run(run())
