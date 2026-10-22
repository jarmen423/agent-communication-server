"""The dev stub control plane mints identities with its own copy of the
identity rules. Keep that copy equal to the Rust source of truth, so a new
op namespace or reserved component name (e.g. `lock`, `hub-locks`) can't be
minted for an agent and then forge that component's traffic."""

import re
from pathlib import Path

from stub_control_plane import identity_rules

ROOT = Path(__file__).resolve().parents[2]


def _rust_str_array(path: Path, const: str) -> frozenset[str]:
    src = path.read_text()
    m = re.search(rf"{const}\s*:\s*&\[&str\]\s*=\s*&?\[(.*?)\];", src, re.S)
    assert m, f"{const} not found in {path}"
    return frozenset(re.findall(r'"([^"]+)"', m.group(1)))


def test_op_namespaces_match_rust():
    rust = _rust_str_array(ROOT / "crates/client/src/query_api.rs", "OP_NAMESPACES")
    assert identity_rules.OP_NAMESPACES == rust


def test_reserved_identities_match_rust():
    rust = _rust_str_array(ROOT / "crates/client/src/protocol.rs", "RESERVED_IDENTITIES")
    assert identity_rules.RESERVED == rust
