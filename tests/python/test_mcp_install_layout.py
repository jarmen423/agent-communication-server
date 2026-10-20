"""Plugin install layout: run each plugin the way its host installs it.

Claude Code and Codex install marketplace plugins into a versioned cache dir,
``~/.claude/plugins/cache/<marketplace>/<name>/<version>/`` and
``~/.codex/plugins/cache/<marketplace>/<name>/<version>/``; Hermes uses
``~/.hermes/plugins/<name>/``. There the parent of ``server/`` is a version
string, not ``claude-code-plugin``, so the default identity has to come from
the host manifest. These tests copy each plugin into such a layout under a
temporary HOME, launch the MCP server over stdio with the command from the
plugin's own ``.mcp.json`` and ``NATS_HUB_IDENTITY`` unset, and check the
default identity and the full tool list. No NATS needed (runs in
.github/workflows/plugins.yml).

``python3`` in ``.mcp.json`` is replaced by this interpreter, which has the
plugin's Python deps (``mcp``, ``nats-py``) installed, as a user's would.
"""
from __future__ import annotations

import asyncio
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "mcp_server"))

import hub_tools  # noqa: E402

EXPECTED_TOOLS = {t.name for t in hub_tools.TOOLS} | {"whoami"}
DEAD_NATS = "nats://127.0.0.1:1"

# plugin dir, cache root under HOME, marketplace name, .mcp.json key,
# root placeholder, expected default identity
LAYOUTS = [
    ("claude-code-plugin", ".claude/plugins/cache", "nats-hub",
     "mcpServers", "${CLAUDE_PLUGIN_ROOT}", "claude-code-agent"),
    ("codex-plugin", ".codex/plugins/cache", "nats-hub-local",
     "mcp_servers", "${PLUGIN_ROOT}", "codex-agent"),
]


def _install(plugin: str, home: Path, cache: str, mkt: str) -> Path:
    root = home / cache / mkt / "nats-hub" / "0.1.0"
    shutil.copytree(REPO_ROOT / plugin, root,
                    ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    return root


def _server_env(home: Path, identity: str | None = None) -> dict[str, str]:
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(home),
        "NATS_URL": DEAD_NATS,
        "NATS_HUB_CONNECT_TIMEOUT": "1",
        "NATS_HUB_CONNECT_RETRIES": "0",  # clamped to 1 (0 = forever in nats-py)
    }
    if identity is not None:
        env["NATS_HUB_IDENTITY"] = identity
    return env


def _launch_cmd(root: Path, key: str, placeholder: str) -> tuple[str, list[str]]:
    spec = json.loads((root / ".mcp.json").read_text())[key]["nats-hub"]
    assert spec["command"] == "python3"
    args = [a.replace(placeholder, str(root)) for a in spec["args"]]
    assert Path(args[0]).is_file(), args
    return sys.executable, args


async def _session_probe(cmd: str, args: list[str], env: dict, cwd: Path) -> dict:
    from mcp import ClientSession, StdioServerParameters
    from mcp.client.stdio import stdio_client

    params = StdioServerParameters(command=cmd, args=args, env=env, cwd=cwd)
    out: dict = {}
    async with stdio_client(params) as (r, w):
        async with ClientSession(r, w) as s:
            await s.initialize()
            tools = await s.list_tools()
            out["tools"] = {t.name for t in tools.tools}
            out["schemas"] = {t.name: t.inputSchema for t in tools.tools}
            who = await s.call_tool("whoami", {})
            out["whoami"] = json.loads(who.content[0].text)
            bad = await s.call_tool("delegate_task", {"to": "w"})
            out["bad_args"] = (bad.isError, bad.content[0].text)
            down = await s.call_tool("list_agents", {})
            out["bus_down"] = (down.isError, down.content[0].text)
    return out


@pytest.mark.parametrize("plugin,cache,mkt,key,placeholder,want", LAYOUTS,
                         ids=[x[0] for x in LAYOUTS])
def test_marketplace_install_layout(tmp_path, plugin, cache, mkt, key,
                                    placeholder, want):
    home = tmp_path / "home"
    root = _install(plugin, home, cache, mkt)
    cmd, args = _launch_cmd(root, key, placeholder)
    out = asyncio.run(asyncio.wait_for(
        _session_probe(cmd, args, _server_env(home), tmp_path), 60))

    assert out["tools"] == EXPECTED_TOOLS
    for name, schema in out["schemas"].items():
        assert schema.get("additionalProperties") is False, name
    who = out["whoami"]["data"]
    assert who["identity"] == want
    assert who["identity_source"] == "plugin-manifest"
    assert who["server_dir"] == str(root / "server")  # the installed copy
    is_err, text = out["bad_args"]
    assert is_err and "'prompt' is a required property" in text
    is_err, text = out["bus_down"]  # fails fast with a next step, no hang
    assert is_err and "cannot reach NATS" in text


def test_env_identity_overrides_manifest(tmp_path):
    home = tmp_path / "home"
    root = _install("claude-code-plugin", home, ".claude/plugins/cache", "m")
    cmd, args = _launch_cmd(root, "mcpServers", "${CLAUDE_PLUGIN_ROOT}")
    out = asyncio.run(asyncio.wait_for(_session_probe(
        cmd, args, _server_env(home, identity="orch-7"), tmp_path), 60))
    assert out["whoami"]["data"]["identity"] == "orch-7"
    assert out["whoami"]["data"]["identity_source"] == "env"


@pytest.mark.parametrize("plugin,cache,mkt", [
    ("claude-code-plugin", ".claude/plugins/cache", "nats-hub"),
    ("codex-plugin", ".codex/plugins/cache", "nats-hub-local"),
])
def test_installed_session_start_hook(tmp_path, plugin, cache, mkt):
    """hooks.json's command, resolved against the install dir, emits the
    SessionStart JSON (the vendored nats_connect is found from hooks/)."""
    home = tmp_path / "home"
    root = _install(plugin, home, cache, mkt)
    hooks = json.loads((root / "hooks" / "hooks.json").read_text())
    command = hooks["hooks"]["SessionStart"][0]["hooks"][0]["command"]
    for ph in ("${CLAUDE_PLUGIN_ROOT}", "${PLUGIN_ROOT}"):
        command = command.replace(ph, str(root))
    assert command.startswith("python3 ")
    script = command.split(" ", 1)[1].strip('"')
    r = subprocess.run([sys.executable, script], input="{}", text=True,
                       capture_output=True, timeout=30, cwd=tmp_path,
                       env=_server_env(home))
    assert r.returncode == 0, r.stderr
    out = json.loads(r.stdout)
    assert out["hookSpecificOutput"]["hookEventName"] == "SessionStart"
    assert "not reachable" in out["hookSpecificOutput"]["additionalContext"]


HERMES_PROBE = r"""
import asyncio, importlib.util, json, os, sys
path = sys.argv[1]
spec = importlib.util.spec_from_file_location("nats_hub_plugin", path)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

class Ctx:
    tools = {}
    def register_tool(self, name, toolset, schema, handler, description):
        self.tools[name] = (schema, handler)
    def register_hook(self, *a): pass
    def register_command(self, *a): pass

ctx = Ctx()
mod.register(ctx)
schema, handler = ctx.tools["nats_hub"]
who = json.loads(handler({"action": "whoami", "params": {}}))
bad = json.loads(handler({"action": "cancel_task", "params": {}}))
print(json.dumps({
    "actions": schema["parameters"]["properties"]["action"]["enum"],
    "whoami": who, "bad": bad,
}))
"""


def test_hermes_install_layout(tmp_path):
    home = tmp_path / "home"
    root = home / ".hermes" / "plugins" / "nats-hub"
    shutil.copytree(REPO_ROOT / "hermes-plugin", root,
                    ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
    r = subprocess.run(
        [sys.executable, "-c", HERMES_PROBE, str(root / "__init__.py")],
        text=True, capture_output=True, timeout=60, cwd=tmp_path,
        env=_server_env(home))
    assert r.returncode == 0, r.stderr
    out = json.loads(r.stdout.strip().splitlines()[-1])
    assert set(out["actions"]) == EXPECTED_TOOLS
    assert out["whoami"]["ok"] and out["whoami"]["data"]["identity"] == "hermes-agent"
    assert out["whoami"]["data"]["server_dir"] == str(root / "server")
    assert not out["bad"]["ok"]
    assert "'task_id' is a required property" in out["bad"]["error"]


def test_plugin_copies_in_sync():
    r = subprocess.run(["bash", "scripts/dev/sync_plugins.sh", "--check"],
                       cwd=REPO_ROOT, capture_output=True, text=True)
    assert r.returncode == 0, r.stderr or r.stdout
