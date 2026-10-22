"""Identity name rules, the same as ``hub-admin``'s ``check_identity``
(``src/bin/hub_admin/acl.rs``): one NATS token of ``[A-Za-z0-9_-]``, not a
query-API op namespace and not a reserved hub component name. The control
plane validates every name it mints, because a scoped signing key accepts
whatever JWT ``name`` it is given (docs/OPERATOR_MODE.md)."""

from __future__ import annotations

import re

_VALID = re.compile(r"^[A-Za-z0-9_-]+$")
# Mirrors crates/client/src/query_api.rs OP_NAMESPACES and
# crates/client/src/protocol.rs RESERVED_IDENTITIES; tests/python/
# test_stub_identity_rules_sync.py fails if they drift.
OP_NAMESPACES = frozenset({"wave", "session", "agent", "history", "thread",
                           "envelope", "stats", "ping", "backup", "lock"})
RESERVED = frozenset({"hub-server", "hub-orchestrator", "hub-locks", "visualizer"})


def check_identity(name: str) -> str:
    if not isinstance(name, str) or not _VALID.match(name):
        raise ValueError(f"invalid identity {name!r}: must be one or more of [A-Za-z0-9_-]")
    if name in OP_NAMESPACES:
        raise ValueError(f"identity {name!r} collides with a query-API op namespace")
    if name in RESERVED:
        raise ValueError(f"identity {name!r} is reserved for a hub component")
    return name
