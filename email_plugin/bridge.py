"""The poll → enrich → gate → label → wake → reply loop (docs/EMAIL_PLUGIN.md §4).

Each email is marked processed *before* its agent is woken, so a crash can
lose a reply but never wakes an agent twice for the same email (tokens are the
scarce resource). Every decision is logged with the email, thread and task ids.
"""
from __future__ import annotations

import asyncio
import logging

from .config import Mailbox, PluginConfig
from .context import build_prompt, reply_text
from .policy import HOLD, IGNORE, WAKE, Counters, InboundEmail, Verdict, evaluate, may_send
from .state import DAY, State

log = logging.getLogger("email_plugin")

LABELS = {  # name → color; visible on threads in the Resend dashboard
    WAKE: ("acs-woke", "#3E63DD"),
    HOLD: ("acs-held", "#F5A524"),
    IGNORE: ("acs-ignored", "#8B8D98"),
    "replied": ("acs-replied", "#30A46C"),
    "drafted": ("acs-drafted", "#30A46C"),
    "failed": ("acs-failed", "#E5484D"),
}
REPLY_HEADERS = {"Auto-Submitted": "auto-replied"}  # RFC 3834: no auto-reply loops
SEND_HEADERS = {"Auto-Submitted": "auto-generated"}


def to_inbound(thread_id: str, msg: dict, received: dict) -> InboundEmail:
    headers = {str(k).lower(): v for k, v in (received.get("headers") or {}).items()}
    return InboundEmail(
        id=msg["id"], thread_id=thread_id, from_raw=msg.get("from") or "",
        subject=msg.get("subject") or "", text=msg.get("text") or "",
        to=tuple(msg.get("to") or ()), received_at=msg.get("received_at") or "",
        auth=received.get("authentication") or {}, headers=headers,
        attachments=tuple(msg.get("attachments") or ()))


class EmailBridge:
    def __init__(self, cfg: PluginConfig, api, hub, state: State):
        self.cfg = cfg
        self.api = api
        self.hub = hub
        self.state = state
        self.tasks: set[asyncio.Task] = set()
        self._thread_locks: dict[str, asyncio.Lock] = {}

    # ── loop ───────────────────────────────────────────────────────────
    async def run(self) -> None:
        if self.hub is not None:
            await self.hub.serve_requests(self.send_for_agent)
        log.info("watching %d mailbox(es) every %ss",
                 len(self.cfg.mailboxes), self.cfg.poll_seconds)
        while True:
            await self.poll_once()
            await asyncio.sleep(self.cfg.poll_seconds)

    async def poll_once(self) -> None:
        for mb in self.cfg.mailboxes:
            try:
                await self._poll_mailbox(mb)
            except Exception:  # noqa: BLE001 - one bad mailbox must not stop the rest
                log.exception("poll failed mailbox=%s", mb.address)
            self.state.save()

    async def drain(self) -> None:
        """Wait for in-flight wakes (tests, shutdown)."""
        while self.tasks:
            await asyncio.gather(*list(self.tasks), return_exceptions=True)

    async def _poll_mailbox(self, mb: Mailbox) -> None:
        threads = await self.api.list_threads(mb.inbox_id)
        baseline = not self.state.baselined(mb.inbox_id)
        for t in threads:
            count = t.get("email_count")
            if count is not None and count == self.state.thread_count(mb.inbox_id, t["id"]):
                continue
            emails = await self.api.list_thread_emails(mb.inbox_id, t["id"])
            for i, msg in enumerate(emails):
                if msg.get("direction") != "inbound" or self.state.is_processed(msg["id"]):
                    continue
                if baseline:  # first sight of this inbox: history never wakes anyone
                    self.state.mark(msg["id"], "baseline", "present when the plugin started")
                    continue
                await self._handle(mb, t["id"], msg, emails[: i + 1])
            self.state.set_thread_count(mb.inbox_id, t["id"], count if count is not None else len(emails))
        if baseline:
            self.state.set_baselined(mb.inbox_id)
            log.info("baselined mailbox=%s threads=%d", mb.address, len(threads))

    # ── one email ──────────────────────────────────────────────────────
    async def _handle(self, mb: Mailbox, thread_id: str, msg: dict, thread: list[dict]) -> None:
        received = await self.api.received_email(msg["id"])
        email = to_inbound(thread_id, msg, received)
        verdict = evaluate(email, mb, own_addresses=self.cfg.own_addresses(), counters=Counters(
            wakes_last_hour=self.state.wakes_within(mb.agent, 3600),
            wakes_last_day=self.state.wakes_within(mb.agent, DAY),
            agent_turns_in_thread=self.state.agent_turns(thread_id)))
        self.state.mark(email.id, verdict.action, verdict.reason, mailbox=mb.address,
                        thread=thread_id, sender=email.from_addr)
        log.info("gate mailbox=%s email=%s thread=%s from=%s action=%s reason=%s",
                 mb.address, email.id, thread_id, email.from_addr, verdict.action, verdict.reason)
        await self._label(mb, thread_id, verdict.action)
        if verdict.action != WAKE:
            return
        self.state.record_wake(mb.agent)
        if verdict.peer:
            self.state.add_agent_turn(thread_id)
        self.state.save()
        task = asyncio.create_task(self._wake(mb, email, thread, verdict))
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)

    async def _wake(self, mb: Mailbox, email: InboundEmail, thread: list[dict],
                    verdict: Verdict) -> None:
        lock = self._thread_locks.setdefault(email.thread_id, asyncio.Lock())
        async with lock:  # one agent run per thread at a time, in arrival order
            prompt = build_prompt(mb, email, thread, verdict)
            outcome = await self.hub.delegate(
                mb.agent, prompt, mb.task_timeout,
                on_start=lambda tid: log.info("woke agent=%s email=%s task=%s",
                                              mb.agent, email.id, tid))
            self.state.update(email.id, task=outcome.task_id, task_status=outcome.status)
            if outcome.status != "done":
                log.warning("task %s agent=%s email=%s: %s",
                            outcome.status, mb.agent, email.id, outcome.error)
                await self._label(mb, email.thread_id, "failed")
                self.state.save()
                return
            body = reply_text(outcome.result)
            if body is None:
                log.info("agent=%s chose not to reply email=%s", mb.agent, email.id)
            else:
                try:
                    if mb.reply_mode == "draft":
                        sent = await self.api.draft_reply(mb.inbox_id, email.thread_id, email.id, body)
                        mark = "drafted"
                    else:
                        sent = await self.api.reply(mb.inbox_id, email.thread_id, email.id,
                                                    body, headers=REPLY_HEADERS)
                        mark = "replied"
                except Exception as e:  # noqa: BLE001
                    log.error("reply failed email=%s task=%s: %s", email.id, outcome.task_id, e)
                    await self._label(mb, email.thread_id, "failed")
                    self.state.save()
                    return
                self.state.update(email.id, reply=sent, reply_mode=mb.reply_mode)
                log.info("%s email=%s task=%s reply=%s", mark, email.id, outcome.task_id, sent)
                await self._label(mb, email.thread_id, mark)
            self.state.save()

    async def _label(self, mb: Mailbox, thread_id: str, key: str) -> None:
        name, color = LABELS[key]
        try:
            await self.api.add_labels(mb.inbox_id, thread_id,
                                      [await self.api.label_id(mb.inbox_id, name, color)])
        except Exception as e:  # noqa: BLE001 - labels are cosmetic
            log.warning("label %s failed thread=%s: %s", name, thread_id, e)

    # ── agent-initiated email (hub DM to the plugin identity) ──────────
    async def send_for_agent(self, agent: str, req: dict) -> str:
        mb = self.cfg.mailbox_for_agent(agent)
        if mb is None:
            raise PermissionError(f"agent {agent} has no mailbox")
        to = req.get("to")
        to = [to] if isinstance(to, str) else list(to or [])
        subject, text = req.get("subject"), req.get("text")
        if not isinstance(subject, str) or not subject.strip() or not isinstance(text, str) or not text.strip():
            raise ValueError("email needs non-empty 'subject' and 'text'")
        if (why := may_send(mb, to)) is not None:
            log.warning("refused send agent=%s: %s", agent, why)
            raise PermissionError(why)
        sent = await self.api.send_new(mb.inbox_id, to, subject.strip(), text, headers=SEND_HEADERS)
        log.info("agent=%s sent email=%s to=%s", agent, sent, to)
        return f"sent {sent} from {mb.address} to {', '.join(to)}"
