"""ACP client-side policy shared by the stdio/HTTP backends.

- We advertise NO ``fs``/``terminal`` client capabilities (not implemented).
- ``session/request_permission`` is answered by picking an offered option by
  *kind*, per ``permission_policy``; if nothing matches, ``cancelled``.
"""

from __future__ import annotations

from typing import Any

PERMISSION_POLICIES = ("allow_once", "allow_always", "reject")
_POLICY_PREFERENCE = {
    "allow_always": ("allow_always", "allow_once"),
    "allow_once": ("allow_once", "allow_always"),
    "reject": ("reject_once", "reject_always"),
}
CLIENT_CAPABILITIES: dict[str, Any] = {
    "fs": {"readTextFile": False, "writeTextFile": False},
    "terminal": False,
}


def choose_permission_option(options: Any, policy: str) -> str | None:
    """Pick an ``optionId`` from ACP permission ``options`` by kind."""
    if not isinstance(options, list):
        return None
    for kind in _POLICY_PREFERENCE.get(policy, ()):
        for opt in options:
            if isinstance(opt, dict) and opt.get("kind") == kind and opt.get("optionId"):
                return str(opt["optionId"])
    return None


def permission_result(params: dict[str, Any], policy: str) -> dict[str, Any]:
    chosen = choose_permission_option((params or {}).get("options"), policy)
    if chosen:
        return {"outcome": {"outcome": "selected", "optionId": chosen}}
    return {"outcome": {"outcome": "cancelled"}}
