"""The gate: decide, without spending a single token, whether an email wakes
its mailbox's agent. Pure functions only, so every rule is unit-testable.

Rule order and rationale: docs/EMAIL_PLUGIN.md §4 step 3.
"""
from __future__ import annotations

import fnmatch
from dataclasses import dataclass, field
from email.utils import parseaddr

from .config import Mailbox

WAKE = "wake"
HOLD = "hold"  # stays in the inbox for a human; the agent is not woken
IGNORE = "ignore"  # automated or unwanted; the agent is not woken

_AUTOMATED_LOCALPARTS = ("mailer-daemon", "postmaster", "noreply", "no-reply",
                         "do-not-reply", "donotreply", "bounce", "bounces")


@dataclass(frozen=True)
class InboundEmail:
    id: str
    thread_id: str
    from_raw: str
    subject: str = ""
    text: str = ""
    to: tuple[str, ...] = ()
    received_at: str = ""
    # From Resend's received-email record: SPF/DKIM/DMARC computed by the
    # receiving server (unforgeable) and the headers (lowercase keys).
    auth: dict = field(default_factory=dict)
    headers: dict = field(default_factory=dict)
    attachments: tuple[dict, ...] = ()

    @property
    def from_addr(self) -> str:
        return parseaddr(self.from_raw)[1].strip().lower()


@dataclass(frozen=True)
class Verdict:
    action: str
    reason: str
    peer: str | None = None  # sending agent, when the mail came from a peer


@dataclass(frozen=True)
class Counters:
    """Rolling usage the gate needs, read from the state file."""
    wakes_last_hour: int = 0
    wakes_last_day: int = 0
    agent_turns_in_thread: int = 0


def matches(addr: str, patterns: tuple[str, ...]) -> bool:
    return any(fnmatch.fnmatchcase(addr, p) for p in patterns)


def automated_reason(email: InboundEmail) -> str | None:
    """Why the email looks machine-sent (RFC 3834 and list headers), if it does."""
    h = email.headers
    auto = str(h.get("auto-submitted", "")).strip().lower()
    if auto and auto != "no":
        return f"Auto-Submitted: {auto}"
    prec = str(h.get("precedence", "")).strip().lower()
    if prec in ("bulk", "list", "junk"):
        return f"Precedence: {prec}"
    for key in ("list-id", "list-unsubscribe", "x-autoreply", "x-autorespond"):
        if h.get(key):
            return f"{key} header"
    local = email.from_addr.split("@", 1)[0]
    if local in _AUTOMATED_LOCALPARTS:
        return f"automated sender {email.from_addr}"
    return None


def authenticated(email: InboundEmail) -> bool:
    """DKIM ``pass`` (Resend: signed by the From domain) or DMARC ``pass``.
    SPF alone vouches only for the envelope sender, not the visible From."""
    return email.auth.get("dkim") == "pass" or email.auth.get("dmarc") == "pass"


def evaluate(email: InboundEmail, mb: Mailbox, *, own_addresses: dict[str, str],
             counters: Counters) -> Verdict:
    sender = email.from_addr
    peer = own_addresses.get(sender)
    if peer is not None:
        if peer == mb.agent:
            return Verdict(IGNORE, "sent by this mailbox itself")
        if peer not in mb.peers:
            return Verdict(IGNORE, f"agent {peer} is not in peers", peer)
    elif (why := automated_reason(email)) is not None:
        return Verdict(IGNORE, why)

    if mb.require_auth and not authenticated(email):
        auth = ", ".join(f"{k}={email.auth.get(k, 'unknown')}" for k in ("spf", "dkim", "dmarc"))
        return Verdict(HOLD, f"sender not authenticated ({auth})", peer)

    if peer is None:
        if matches(sender, mb.deny_from):
            return Verdict(IGNORE, f"{sender} matches deny_from")
        if not matches(sender, mb.allow_from):
            return Verdict(HOLD, f"{sender} is not in allow_from")
        if mb.subject_prefix and not email.subject.startswith(mb.subject_prefix):
            return Verdict(HOLD, f"subject lacks prefix {mb.subject_prefix!r}")
    elif counters.agent_turns_in_thread >= mb.max_agent_turns:
        return Verdict(HOLD, f"agent turns in thread reached {mb.max_agent_turns}", peer)

    if counters.wakes_last_hour >= mb.max_wakes_per_hour:
        return Verdict(HOLD, f"hourly wake budget ({mb.max_wakes_per_hour}) used", peer)
    if counters.wakes_last_day >= mb.max_wakes_per_day:
        return Verdict(HOLD, f"daily wake budget ({mb.max_wakes_per_day}) used", peer)
    return Verdict(WAKE, "peer agent" if peer else f"{sender} is allowed", peer)


def may_send(mb: Mailbox, recipients: list[str]) -> str | None:
    """None when the agent may email every recipient, else the reason not."""
    if not recipients:
        return "no recipients"
    for r in recipients:
        addr = parseaddr(r)[1].strip().lower()
        if not addr or not matches(addr, mb.may_email):
            return f"{r} is not in may_email for {mb.agent}"
    return None
