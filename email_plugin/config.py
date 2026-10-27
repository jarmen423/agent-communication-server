"""Plugin config: one TOML file, one ``[[mailbox]]`` table per agent address.

Every policy knob the gate reads lives on ``Mailbox`` so a future control-plane
UI can write the same fields. ``example.toml`` documents each one.
"""
from __future__ import annotations

import json
import os
import re
import tomllib
from dataclasses import dataclass, field, fields
from pathlib import Path

from nats_connect import validate_identity

CONTEXT_MODES = ("message", "thread")
CONVERSATION_MODES = ("session", "oneshot")
REPLY_MODES = ("per_sender", "send", "draft")
_ADDRESS_RE = re.compile(r"^[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]+\.[A-Za-z]{2,}$")


class ConfigError(ValueError):
    pass


@dataclass(frozen=True)
class Mailbox:
    address: str
    agent: str
    inbox_id: str
    # Who may wake the agent (fnmatch patterns on the lowercase address).
    allow_from: tuple[str, ...] = ()
    deny_from: tuple[str, ...] = ()
    # Require DKIM or DMARC pass, so an allow_from match can't be spoofed.
    require_auth: bool = True
    subject_prefix: str = ""
    # Agent-to-agent: agents whose mail may wake this one, addresses this
    # agent may send to, and the per-thread cap on peer-triggered wakes.
    peers: tuple[str, ...] = ()
    may_email: tuple[str, ...] = ()
    max_agent_turns: int = 4
    # Token budget: wakes per rolling hour / day.
    max_wakes_per_hour: int = 10
    max_wakes_per_day: int = 50
    # What the agent sees.
    context: str = "thread"
    max_context_chars: int = 20_000
    instructions: str = ""
    # session: one hub session per email thread, the worker keeps memory
    # between emails (D3); oneshot: one task per email, thread text as context.
    conversation: str = "session"
    # A thread's session is closed after this long without a turn; the next
    # email starts a fresh one with the thread as context. 0 = never.
    session_idle_hours: float = 168.0
    # per_sender (D7): send replies to authenticated senders matching
    # auto_send_from (empty: allow_from without a bare "*") and to peer
    # agents, save a draft for everyone else. send / draft: always.
    reply_mode: str = "per_sender"
    auto_send_from: tuple[str, ...] = ()
    task_timeout: float = 600.0


@dataclass(frozen=True)
class PluginConfig:
    resend_api_key_file: str
    state_path: str
    identity: str = "email-bridge"
    resend_api_url: str = "https://api.resend.com"
    poll_seconds: float = 15.0
    nats_url: str = ""
    mailboxes: tuple[Mailbox, ...] = field(default_factory=tuple)

    def api_key(self) -> str:
        key = Path(os.path.expanduser(self.resend_api_key_file)).read_text().strip()
        if not key:
            raise ConfigError(f"{self.resend_api_key_file} is empty")
        return key

    def own_addresses(self) -> dict[str, str]:
        """address → agent, for every mailbox this plugin manages."""
        return {m.address: m.agent for m in self.mailboxes}

    def mailbox_for_agent(self, agent: str) -> Mailbox | None:
        return next((m for m in self.mailboxes if m.agent == agent), None)


def valid_address(addr: str) -> bool:
    return bool(_ADDRESS_RE.match(addr))


def _mailbox(raw: dict, where: str) -> Mailbox:
    known = {f.name for f in fields(Mailbox)}
    unknown = set(raw) - known
    if unknown:
        raise ConfigError(f"{where}: unknown keys {sorted(unknown)}")
    for key in ("address", "agent", "inbox_id"):
        if not isinstance(raw.get(key), str) or not raw[key]:
            raise ConfigError(f"{where}: '{key}' is required")
    vals = dict(raw)
    vals["address"] = raw["address"].strip().lower()
    for key in ("allow_from", "deny_from", "may_email", "auto_send_from"):
        vals[key] = tuple(p.strip().lower() for p in raw.get(key, ()))
    vals["peers"] = tuple(raw.get("peers", ()))
    mb = Mailbox(**vals)
    if not valid_address(mb.address):
        raise ConfigError(f"{where}: bad address {mb.address!r}")
    try:
        validate_identity(mb.agent)
        for p in mb.peers:
            validate_identity(p)
    except ValueError as e:
        raise ConfigError(f"{where}: {e}") from None
    if mb.context not in CONTEXT_MODES:
        raise ConfigError(f"{where}: context must be one of {CONTEXT_MODES}")
    if mb.reply_mode not in REPLY_MODES:
        raise ConfigError(f"{where}: reply_mode must be one of {REPLY_MODES}")
    if mb.conversation not in CONVERSATION_MODES:
        raise ConfigError(f"{where}: conversation must be one of {CONVERSATION_MODES}")
    for key in ("session_idle_hours", "task_timeout"):
        v = getattr(mb, key)
        if isinstance(v, bool) or not isinstance(v, (int, float)) or v < 0:
            raise ConfigError(f"{where}: {key} must be a non-negative number")
    for key in ("max_agent_turns", "max_wakes_per_hour", "max_wakes_per_day",
                "max_context_chars"):
        if not isinstance(getattr(mb, key), int) or getattr(mb, key) < 0:
            raise ConfigError(f"{where}: {key} must be a non-negative integer")
    return mb


def load_config(path: str | os.PathLike) -> PluginConfig:
    with open(path, "rb") as f:
        raw = tomllib.load(f)
    boxes = raw.pop("mailbox", [])
    known = {f.name for f in fields(PluginConfig)} - {"mailboxes"}
    unknown = set(raw) - known
    if unknown:
        raise ConfigError(f"{path}: unknown keys {sorted(unknown)}")
    for key in ("resend_api_key_file", "state_path"):
        if not raw.get(key):
            raise ConfigError(f"{path}: '{key}' is required")
    mailboxes = tuple(_mailbox(b, f"{path} mailbox[{i}]") for i, b in enumerate(boxes))
    for attr in ("address", "agent"):
        seen = [getattr(m, attr) for m in mailboxes]
        dupes = {v for v in seen if seen.count(v) > 1}
        if dupes:
            raise ConfigError(f"{path}: duplicate mailbox {attr}: {sorted(dupes)}")
    cfg = PluginConfig(**raw, mailboxes=mailboxes)
    try:
        validate_identity(cfg.identity)
    except ValueError as e:
        raise ConfigError(f"{path}: {e}") from None
    return cfg


def _toml_value(v) -> str:
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, (int, float)):
        return repr(v)
    if isinstance(v, (list, tuple)):
        return "[" + ", ".join(_toml_value(x) for x in v) + "]"
    return json.dumps(str(v))  # JSON string escapes are valid TOML basic strings


def mailbox_toml(mb: Mailbox) -> str:
    """A ``[[mailbox]]`` block with only the fields that differ from defaults."""
    default = Mailbox(address="", agent="", inbox_id="")
    lines = ["[[mailbox]]"]
    for f in fields(Mailbox):
        v = getattr(mb, f.name)
        if f.name in ("address", "agent", "inbox_id") or v != getattr(default, f.name):
            lines.append(f"{f.name} = {_toml_value(v)}")
    return "\n".join(lines) + "\n"


def write_initial_config(path: Path, *, key_file: str, state_path: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "# email_plugin config (docs/EMAIL_PLUGIN.md, email_plugin/example.toml)\n"
        f"resend_api_key_file = {_toml_value(key_file)}\n"
        f"state_path = {_toml_value(state_path)}\n"
        'identity = "email-bridge"\n'
        "poll_seconds = 15\n"
    )


def append_mailbox(path: Path, mb: Mailbox) -> None:
    with open(path, "a") as f:
        f.write("\n" + mailbox_toml(mb))
    load_config(path)  # fail loudly if the result doesn't parse
