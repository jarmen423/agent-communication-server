"""Remote MCP endpoint (resource server), in-process; no NATS needed.

The live path (OAuth dance, two replicas, durable hub) is
test_remote_mcp_e2e.py on the operator stack.
"""
import asyncio
import json
import time

import httpx
import jwt
import pytest

import mcp_remote  # noqa: F401  (puts mcp_server/ on sys.path)
import hub_connection as conn
from mcp_remote.app import create_app
from mcp_remote.handlers import HANDLERS
from mcp_remote.paging import page_text
from mcp_remote.tools import TOOLS, WAIT_CAP
from stub_control_plane.tokens import TokenIssuer

ISSUER = "http://127.0.0.1:9300"
RESOURCE = "http://mcp.test/mcp"
METADATA = "http://mcp.test/.well-known/oauth-protected-resource/mcp"


def app_and_issuer():
    # cp_url points nowhere: credential fetches fail fast, as a tool error.
    app = create_app(resource_url=RESOURCE, issuer=ISSUER, nats_url="nats://127.0.0.1:1",
                     cp_url="http://127.0.0.1:1", service_key="s" * 43)
    issuer = TokenIssuer(ISSUER)
    app.state.verifier.load_jwks(issuer.jwks())
    return app, issuer


def mint(issuer, **over):
    args = dict(subject="dev-user", client_id="c1", audience=RESOURCE, scopes=["acs:hub"],
                tenant="T_A", identity="claude-web-7", ttl_secs=600)
    args.update(over)
    return issuer.mint(**args).token


def client(app, token=None):
    headers = {"Authorization": f"Bearer {token}"} if token else {}
    return httpx.AsyncClient(transport=httpx.ASGITransport(app=app),
                             base_url="http://mcp.test", headers=headers)


INIT = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
    "protocolVersion": "2025-06-18", "capabilities": {},
    "clientInfo": {"name": "t", "version": "0"}}}
ACCEPT = {"Accept": "application/json, text/event-stream", "Content-Type": "application/json"}


def test_unauthenticated_gets_401_with_resource_metadata():
    async def go():
        app, issuer = app_and_issuer()
        other = TokenIssuer(ISSUER)  # same issuer URL, unknown key
        bad_tokens = [None, "not-a-jwt", mint(issuer, audience="http://other.test/mcp"),
                      mint(issuer, ttl_secs=-60), mint(issuer, scopes=["other"]),
                      mint(other)]
        for tok in bad_tokens:
            async with client(app, tok) as http:
                resp = await http.post("/mcp", json=INIT, headers=ACCEPT)
            assert resp.status_code in (401, 403), (tok, resp.status_code)
            www = resp.headers["www-authenticate"]
            assert www.startswith("Bearer ") and f'resource_metadata="{METADATA}"' in www
    asyncio.run(go())


def test_protected_resource_metadata():
    async def go():
        app, _ = app_and_issuer()
        async with client(app) as http:
            for path in ("/.well-known/oauth-protected-resource/mcp",
                         "/.well-known/oauth-protected-resource"):
                doc = (await http.get(path)).json()
                assert doc["resource"] == RESOURCE
                assert [a.rstrip("/") for a in doc["authorization_servers"]] == [ISSUER]
                assert doc["scopes_supported"] == ["acs:hub"]
                assert doc["bearer_methods_supported"] == ["header"]
    asyncio.run(go())


def _rpc(resp):
    text = resp.text
    if resp.headers.get("content-type", "").startswith("text/event-stream"):
        text = next(line[5:] for line in text.splitlines() if line.startswith("data:"))
    return json.loads(text)


def test_authenticated_tools_list_and_identity_from_token():
    async def go():
        app, issuer = app_and_issuer()
        tok = mint(issuer)
        async with app.router.lifespan_context(app), client(app, tok) as http:
            assert (await http.post("/mcp", json=INIT, headers=ACCEPT)).status_code == 200
            tools = _rpc(await http.post("/mcp", headers=ACCEPT, json={
                "jsonrpc": "2.0", "id": 2, "method": "tools/list"}))["result"]["tools"]
            names = {t["name"] for t in tools}
            assert {"delegate", "get_task_result", "read_inbox", "cancel_task",
                    "start_session", "create_wave", "spawn_wave"} <= names
            assert not names & {"delegate_task", "wait_for_task", "wait_for_message"}

            async def call(name, args):
                body = _rpc(await http.post("/mcp", headers=ACCEPT, json={
                    "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": {"name": name, "arguments": args}}))["result"]
                return body["isError"], json.loads(body["content"][0]["text"])

            err, who = await call("whoami", {})
            assert not err and who["data"]["identity"] == "claude-web-7"
            assert who["data"]["tenant"] == "T_A"
            err, out = await call("send_direct", {"to": "bob", "message": "x",
                                                  "from": "claude-web-1"})
            assert err and "authenticated identity" in out["error"]
            # Bus tools need credentials; an unreachable control plane is a tool
            # error, not a crash or a hang.
            t0 = time.monotonic()
            err, out = await call("list_agents", {})
            assert err and "control plane" in out["error"] and time.monotonic() - t0 < 15
    asyncio.run(go())


def test_every_tool_has_title_and_hints():
    for tool in TOOLS:
        a = tool.annotations
        assert tool.title and a.title == tool.title, tool.name
        assert a.readOnlyHint is not None and a.destructiveHint is not None, tool.name
        if a.readOnlyHint:
            assert not a.destructiveHint, tool.name
    by = {t.name: t for t in TOOLS}
    for name in ("get_task_result", "read_inbox", "session_replies", "list_agents",
                 "get_history", "wave_status", "whoami"):
        assert by[name].annotations.readOnlyHint, name
    for name in ("cancel_task", "cancel_wave", "close_session"):
        assert by[name].annotations.destructiveHint, name


def test_waits_are_turn_bounded():
    assert WAIT_CAP < 60
    for tool in TOOLS:
        if tool.name == "spawn_wave":
            continue  # `timeout` is the wave's lifetime on hub-server; the call returns at once
        props = tool.inputSchema.get("properties", {})
        for key in ("wait_secs", "timeout"):
            if key in props:
                assert props[key]["maximum"] <= WAIT_CAP, (tool.name, key)


def test_ids_cannot_inject_subject_tokens():
    async def go():
        async def no_nc():
            raise AssertionError("must fail validation before touching NATS")

        with conn.bound("claude-web-7", no_nc):
            for name, args in (
                ("get_task_result", {"task_id": "abcd1234.>", "worker": "w"}),
                ("get_task_result", {"task_id": "abcd1234", "worker": "w.*"}),
                ("cancel_task", {"task_id": "*", "worker": "w"}),
                ("session_replies", {"session_id": "a.b"}),
                ("delegate", {"to": "inbox.>", "prompt": "x"}),
                ("get_task_result", {"task_id": "abcd1234", "worker": "w", "wait_secs": 600}),
            ):
                out = await HANDLERS[name](args)
                assert not out["ok"] and "invalid arguments" in out["error"], (name, out)
    asyncio.run(go())


def test_bound_identity_overrides_env(monkeypatch):
    monkeypatch.setenv("NATS_HUB_IDENTITY", "env-agent")

    async def nc():
        return None

    assert conn.identity() == "env-agent"
    with conn.bound("claude-web-3", nc):
        assert conn.identity() == "claude-web-3"
        assert conn.envelope("dm", {})["meta"]["from"] == "claude-web-3"
        with conn.bound("other-1", nc):
            assert conn.identity() == "other-1"
        assert conn.identity() == "claude-web-3"
    assert conn.identity() == "env-agent" and not conn.is_bound()
    with pytest.raises(ValueError):
        with conn.bound("bad.identity", nc):
            pass


@pytest.mark.parametrize("text", [
    "a" * 250_000,
    ('"\\\n\t' * 40_000) + "tail",  # every char escapes to 2
    "\x01" * 30_000 + "ü" * 30_000,  # \u0001 escapes to 6; ü stays 1
])
def test_paging_bounds_escaped_size_and_reassembles(text):
    pages, cursor = [], 0
    while True:
        page = page_text(text, cursor, 100_000)
        assert len(json.dumps(page["text"], ensure_ascii=False)) - 2 <= 100_000
        assert page["total_chars"] == len(text) and page["offset"] == cursor
        pages.append(page["text"])
        if page["next_cursor"] is None:
            break
        cursor = int(page["next_cursor"])
    assert "".join(pages) == text and len(pages) >= 2


def test_token_without_identity_claims_is_rejected():
    async def go():
        app, issuer = app_and_issuer()
        now = int(time.time())
        raw = jwt.encode({"iss": ISSUER, "sub": "u", "aud": RESOURCE, "iat": now,
                          "exp": now + 60, "scope": "acs:hub", "jti": "j"},
                         issuer._key, algorithm="EdDSA", headers={"kid": issuer.kid})
        assert await app.state.verifier.verify_token(raw) is None
        assert await app.state.verifier.verify_token(mint(issuer, identity="a.b")) is None
        assert (await app.state.verifier.verify_token(mint(issuer))).identity == "claude-web-7"
    asyncio.run(go())


class _FakeStream:
    """Answers $JS.API.DIRECT.GET like a stream holding ``msgs``."""

    def __init__(self, msgs, durable=True):
        self.msgs, self.durable = msgs, durable  # [(subject, env)]

    async def request(self, subject, data, timeout=None):
        import nats.errors

        if not self.durable:
            raise nats.errors.NoRespondersError
        req = json.loads(data)
        for seq, (subj, env) in enumerate(self.msgs, start=1):
            if seq >= req["seq"] and subj == req["next_by_subj"]:
                return type("M", (), {"headers": {"Nats-Sequence": str(seq)},
                                      "data": json.dumps(env).encode()})
        return type("M", (), {"headers": {"Status": "404"}, "data": b""})


def _env(sender, kind, payload, reply_to=None):
    return {"meta": {"id": f"{sender}-{kind}-{time.time_ns()}", "from": sender,
                     "kind": kind, "reply_to": reply_to, "channel": "task.abcd1234"},
            "payload": payload}


def test_task_view_reads_stream_and_ignores_forged_results():
    from mcp_remote import stored

    task_id = "abcd1234-0000-0000-0000-000000000000"
    subj = "channel.task.abcd1234"
    msgs = [
        ("channel.task.other", _env("w1", "message", {"status": "done"}, task_id)),
        (subj, _env("w1", "status", {"status": "working"})),
        (subj, _env("mallory", "message", {"status": "done", "result": "forged"}, task_id)),
        (subj, _env("w1", "event", {"event_type": "progress"})),
    ]

    async def go():
        nc = _FakeStream(msgs)
        view = stored.TaskView(task_id, "w1")
        await view.update(nc)
        assert view.state == "running" and view.last_status == "working"
        assert len(view.events) == 2 and view.result is None
        msgs.append((subj, _env("w1", "message", {"status": "error", "error": "boom",
                                                  "task_id": task_id})))
        await view.wait(nc, 2, poll=0.05)
        assert view.state == "error" and view.result["payload"]["error"] == "boom"
        with pytest.raises(stored.NotDurable, match="--durable"):
            await stored.TaskView(task_id, "w1").update(_FakeStream([], durable=False))
    asyncio.run(go())


def test_new_dirs_are_client_side_apache():
    """mcp_remote/ and stub_control_plane/ fall under REUSE.toml's default
    (Apache-2.0), using test_licensing.py's own path resolver."""
    import importlib.util
    from pathlib import Path

    path = Path(__file__).with_name("test_licensing.py")
    spec = importlib.util.spec_from_file_location("_licensing", path)
    lic = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(lic)
    for rel in ("mcp_remote/app.py", "mcp_remote/stored.py", "stub_control_plane/app.py",
                "stub_control_plane/nats_issuer.py", "scripts/dev/remote_mcp_demo.sh",
                "docs/REMOTE_MCP.md", "mcp_server/hub_app.py"):
        assert lic._license_for(rel) == "Apache-2.0", rel
