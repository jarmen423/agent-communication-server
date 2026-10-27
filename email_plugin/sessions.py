"""One hub session per email thread (D3, docs/EMAIL_PLUGIN.md §4.3).

The first email the gate lets through in a thread starts a session with the
mailbox's agent (the prompt carries the thread, like a one-shot task). Later
emails in that thread are turns in the same session, so the worker keeps its
memory; each such turn only carries what the session hasn't been shown yet.

A session is replaced by a fresh one (thread text as context again) when:

- the worker says it doesn't hold it (``unknown session``: restarted without
  resume, or closed by someone else);
- it never completed a turn, or its last turn timed out (a worker that doesn't
  answer may not hold it, and the next email shouldn't wait on it again);
- it was idle for ``session_idle_hours`` (closed by :meth:`sweep`), or the
  mailbox now points at another agent.
"""
from __future__ import annotations

import logging
import time
import uuid
from typing import Callable, Iterable

from .config import Mailbox
from .context import build_followup_prompt, build_prompt
from .hub import TaskOutcome
from .policy import InboundEmail, Verdict
from .state import State

log = logging.getLogger("email_plugin.sessions")


def new_session_id() -> str:
    return uuid.uuid4().hex[:16]


class ThreadSessions:
    def __init__(self, hub, state: State, clock=time.time):
        self.hub = hub
        self.state = state
        self.clock = clock

    def _stale(self, mb: Mailbox, rec: dict) -> str | None:
        """Why the thread's session can't take the next turn, if it can't."""
        if rec.get("agent") != mb.agent:
            return f"mailbox now wakes {mb.agent}, not {rec.get('agent')}"
        if not rec.get("turns"):
            return "it never completed a turn"
        return self._idle(mb, rec)

    def _idle(self, mb: Mailbox, rec: dict) -> str | None:
        idle = self.clock() - rec.get("last_turn", 0)
        if mb.session_idle_hours and idle > mb.session_idle_hours * 3600:
            return f"idle for {idle / 3600:.1f}h"
        return None

    async def _close(self, inbox_id: str, thread_id: str, why: str) -> None:
        rec = self.state.drop_thread_session(inbox_id, thread_id)
        if rec is None:
            return
        log.info("closing session=%s agent=%s thread=%s: %s",
                 rec["session_id"], rec["agent"], thread_id, why)
        try:
            await self.hub.close_session(rec["agent"], rec["session_id"])
        except Exception as e:  # noqa: BLE001 - the session is forgotten either way
            log.warning("session_close %s failed: %s", rec["session_id"], e)

    async def turn(self, mb: Mailbox, email: InboundEmail, thread: list[dict],
                   verdict: Verdict, on_start: Callable[[str], None] | None = None) -> TaskOutcome:
        """Run this email as a turn in its thread's session (starting one if
        needed). Callers hold the thread's lock: one turn per thread at a time."""
        inbox, tid = mb.inbox_id, email.thread_id
        rec = self.state.thread_session(inbox, tid)
        if rec is not None and (why := self._stale(mb, rec)) is not None:
            await self._close(inbox, tid, why)
            rec = None
        if rec is not None:
            prompt = build_followup_prompt(mb, email, thread, set(rec["seen"]), verdict)
            outcome = await self.hub.send_session(mb.agent, rec["session_id"], prompt,
                                                  mb.task_timeout, on_start=on_start)
            if not outcome.session_gone:
                return await self._after(mb, email, thread, outcome)
            await self._close(inbox, tid, f"worker lost it ({outcome.error})")

        session_id = new_session_id()
        self.state.set_thread_session(inbox, tid, session_id, mb.agent)
        self.state.save()  # a crash mid-turn still leaves the thread's session known
        log.info("starting session=%s agent=%s thread=%s", session_id, mb.agent, tid)
        outcome = await self.hub.start_session(
            mb.agent, session_id, build_prompt(mb, email, thread, verdict), mb.task_timeout,
            on_start=on_start,
            metadata={"source": "email", "mailbox": mb.address, "thread_id": tid})
        return await self._after(mb, email, thread, outcome)

    async def _after(self, mb: Mailbox, email: InboundEmail, thread: list[dict],
                     outcome: TaskOutcome) -> TaskOutcome:
        done = outcome.status == "done"
        self.state.session_turn(mb.inbox_id, email.thread_id, done=done,
                                seen=[m["id"] for m in thread if m.get("id")])
        rec = self.state.thread_session(mb.inbox_id, email.thread_id)
        if rec is not None and not done and (outcome.status == "timeout" or not rec["turns"]):
            await self._close(mb.inbox_id, email.thread_id, f"turn {outcome.status}")
        return outcome

    async def sweep(self, mailboxes: Iterable[Mailbox], busy: Callable[[str], bool]) -> None:
        """Close sessions idle past their mailbox's ``session_idle_hours`` or
        whose mailbox is gone, so workers don't hold them forever."""
        by_inbox = {mb.inbox_id: mb for mb in mailboxes}
        for inbox, tid, rec in self.state.thread_sessions():
            if busy(tid):
                continue
            mb = by_inbox.get(inbox)
            if mb is None:
                await self._close(inbox, tid, "its mailbox is no longer configured")
            elif (why := self._idle(mb, rec)) is not None:
                await self._close(inbox, tid, why)
