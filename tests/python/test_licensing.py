"""License declarations agree with REUSE.toml (the per-path license map).

Offline and stdlib-only. `reuse lint` (FSFE) checks that every file is covered;
this checks that the coverage says what LICENSING.md promises, that the
license texts are the pinned SPDX ones, and that Cargo.toml and the plugin
manifests declare the matching licenses.
"""

from __future__ import annotations

import hashlib
import json
import re
import tomllib
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]

# SPDX license-list-data v3.29.0, text/<id>.txt.
SPDX_SHA256 = {
    "Apache-2.0": "074e6e32c86a4c0ef8b3ed25b721ca23aca83df277cd88106ef7177c354615ff",
    "BUSL-1.1": "d90560217049db1d6020ec7cf482da4198bfa372dffd22270ba08fd8088cd93b",
}
# LICENSES/BUSL-1.1.txt = our Parameters block + this separator + the pinned SPDX text.
BUSL_SEPARATOR = "\n" + "-" * 77 + "\n\n"

APACHE, BUSL = "Apache-2.0", "BUSL-1.1"
EXPECTED = {
    # client: Python, plugins, installers
    "mcp_server/nats_hub_mcp.py": APACHE,
    "claude-code-plugin/server/nats_hub_mcp.py": APACHE,
    "claude-code-plugin/.claude-plugin/plugin.json": APACHE,
    "hermes-plugin/plugin.yaml": APACHE,
    ".claude-plugin/marketplace.json": APACHE,
    "worker_runtime.py": APACHE,
    "claude_worker.py": APACHE,
    "worker_backends/headless_cli.py": APACHE,
    "remote_agent_adapter.py": APACHE,
    "hub_delivery.py": APACHE,
    "telegram_bridge.py": APACHE,
    "packaging/remote/install.sh": APACHE,
    "scripts/install_remote.sh": APACHE,
    "config/provider_models.json": APACHE,
    # client: Rust
    "src/lib.rs": APACHE,
    "src/protocol.rs": APACHE,
    "src/client.rs": APACHE,
    "src/client/durable.rs": APACHE,
    "src/connect_opts.rs": APACHE,
    "src/query_api_client.rs": APACHE,
    "src/events/mod.rs": APACHE,
    "src/tui/ui/feed.rs": APACHE,
    "src/bin/hub_delegate.rs": APACHE,
    "src/bin/hub_worker/exec.rs": APACHE,
    "src/bin/hub_tui.rs": APACHE,
    # server
    "src/router.rs": BUSL,
    "src/router/mirror.rs": BUSL,
    "src/query_api.rs": BUSL,
    "src/query_api/handlers/wave.rs": BUSL,
    "src/orchestrator/engine.rs": BUSL,
    "src/storage/surreal.rs": BUSL,
    "src/analytics/metrics.rs": BUSL,
    "src/ws_bridge/ws.rs": BUSL,
    "src/wave/validate.rs": BUSL,
    "src/delivery/attach.rs": BUSL,
    "src/bin/hub_server.rs": BUSL,
    "src/bin/hub_admin.rs": BUSL,
    "src/bin/hub_admin/acl.rs": BUSL,
    "visualizer/index.html": BUSL,
    "deploy/systemd/nats-hub-server.service": BUSL,
    "config/nats-server.prod.conf.example": BUSL,
    "src/some_future_module.rs": BUSL,  # new Rust defaults to server side
    # everything else
    "README.md": APACHE,
    "docs/HOSTED_PLAN.md": APACHE,
    "tests/e2e_delegation.rs": APACHE,
    "scripts/dev/with_stack.sh": APACHE,
    "packaging/release/package.sh": APACHE,
    "Cargo.toml": APACHE,
    "some_future_worker.py": APACHE,
}


def _glob_re(pattern: str) -> re.Pattern[str]:
    """REUSE.toml glob: `**` crosses directories, `*` doesn't, the rest is literal."""
    out = []
    i = 0
    while i < len(pattern):
        if pattern.startswith("**", i):
            out.append(".*")
            i += 2
        elif pattern[i] == "*":
            out.append("[^/]*")
            i += 1
        else:
            out.append(re.escape(pattern[i]))
            i += 1
    return re.compile("".join(out))


def _annotations() -> list[dict]:
    data = tomllib.loads((REPO / "REUSE.toml").read_text())
    assert data["version"] == 1
    return data["annotations"]


def _license_for(rel: str) -> str | None:
    found = None
    for ann in _annotations():  # the last matching annotation wins
        paths = ann["path"] if isinstance(ann["path"], list) else [ann["path"]]
        if any(_glob_re(p).fullmatch(rel) for p in paths):
            found = ann["SPDX-License-Identifier"]
    return found


@pytest.mark.parametrize("rel,want", sorted(EXPECTED.items()))
def test_reuse_map(rel: str, want: str) -> None:
    assert _license_for(rel) == want


def test_every_annotation_is_overriding_and_has_a_license_file() -> None:
    for ann in _annotations():
        assert ann.get("precedence") == "override", ann["path"]
        assert (REPO / "LICENSES" / f"{ann['SPDX-License-Identifier']}.txt").is_file()


def test_license_texts_are_pinned_spdx() -> None:
    apache = (REPO / "LICENSES/Apache-2.0.txt").read_bytes()
    assert hashlib.sha256(apache).hexdigest() == SPDX_SHA256[APACHE]

    params, sep, spdx = (REPO / "LICENSES/BUSL-1.1.txt").read_text().partition(BUSL_SEPARATOR)
    assert sep, "separator between the Parameters block and the SPDX text is missing"
    assert hashlib.sha256(spdx.encode()).hexdigest() == SPDX_SHA256[BUSL]
    assert "Licensor:             Agent Memory Labs" in params
    assert "The Licensed Work is (c) 2026 Agent Memory Labs" in params
    assert "designated\n                      BUSL-1.1 in LICENSING.md" in params
    assert "hosted or managed service" in params
    assert "Change Date:          Four years from the date" in params
    assert "Change License:       Apache License, Version 2.0 (Apache-2.0)" in params


def test_manifests_declare_the_split() -> None:
    cargo = tomllib.loads((REPO / "Cargo.toml").read_text())
    assert cargo["package"]["license"] == "Apache-2.0 AND BUSL-1.1"
    for rel in ("claude-code-plugin/.claude-plugin/plugin.json", "codex-plugin/.codex-plugin/plugin.json"):
        assert json.loads((REPO / rel).read_text())["license"] == APACHE, rel
    assert "license: Apache-2.0" in (REPO / "hermes-plugin/plugin.yaml").read_text().splitlines()


def test_licensing_md_names_every_server_glob() -> None:
    doc = (REPO / "LICENSING.md").read_text()
    for ann in _annotations():
        if ann["SPDX-License-Identifier"] != BUSL:
            continue
        for p in ann["path"] if isinstance(ann["path"], list) else [ann["path"]]:
            assert f"`{p}`" in doc, f"LICENSING.md doesn't mention BUSL path {p}"


def test_old_single_license_artifacts_are_gone() -> None:
    assert not (REPO / "LICENSE.md").exists()
    assert not (REPO / "packaging/license/set_license.py").exists()
    for rel in ("LICENSE", "NOTICE", "LICENSING.md"):
        assert "Agent Memory Labs" in (REPO / rel).read_text(), rel
