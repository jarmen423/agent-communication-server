"""packaging/license/set_license.py switches every declaration in one step.

Runs against a scratch copy of the files it touches (NATS_HUB_REPO_ROOT), so the
real tree is never modified. MIT only: it needs no network.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / "packaging" / "license" / "set_license.py"
TOUCHED = [
    "Cargo.toml",
    "claude-code-plugin/.claude-plugin/plugin.json",
    "codex-plugin/.codex-plugin/plugin.json",
    "hermes-plugin/plugin.yaml",
    "README.md",
    "LICENSE.md",
]


def _run(root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    env = dict(os.environ, NATS_HUB_REPO_ROOT=str(root))
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args], env=env, capture_output=True, text=True, check=False
    )


def _scratch_tree(tmp_path: Path) -> Path:
    root = tmp_path / "tree"
    for rel in TOUCHED:
        (root / rel).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(REPO / rel, root / rel)
    return root


def test_dry_run_writes_nothing(tmp_path: Path) -> None:
    root = _scratch_tree(tmp_path)
    before = {rel: (root / rel).read_bytes() for rel in TOUCHED}
    r = _run(root, "MIT", "--dry-run")
    assert r.returncode == 0, r.stderr
    assert "would make" in r.stdout
    assert {rel: (root / rel).read_bytes() for rel in TOUCHED} == before
    assert not (root / "LICENSE").exists()


def test_switch_to_mit_updates_every_declaration(tmp_path: Path) -> None:
    root = _scratch_tree(tmp_path)
    r = _run(root, "MIT", "--holder", "Test Holder", "--year", "2026")
    assert r.returncode == 0, r.stderr

    assert 'license = "MIT"' in (root / "Cargo.toml").read_text()
    for manifest in TOUCHED[1:3]:
        assert json.loads((root / manifest).read_text())["license"] == "MIT"
    assert "license: MIT" in (root / "hermes-plugin/plugin.yaml").read_text().splitlines()

    readme = (root / "README.md").read_text()
    section = readme.split("<!-- license:start -->")[1].split("<!-- license:end -->")[0]
    assert "MIT" in section and "BSL" not in section

    lic = (root / "LICENSE").read_text()
    assert lic.startswith("MIT License") and "Copyright (c) 2026 Test Holder" in lic
    assert not (root / "LICENSE.md").exists()

    # Idempotent: a second run changes nothing.
    again = _run(root, "MIT", "--holder", "Test Holder", "--year", "2026")
    assert again.returncode == 0, again.stderr
    assert "make 0 change(s)" in again.stdout


def test_bsl_requires_parameters(tmp_path: Path) -> None:
    r = _run(_scratch_tree(tmp_path), "BSL-1.1", "--dry-run")
    assert r.returncode != 0
    assert "--change-date" in r.stderr
