"""email_plugin: the token-free gate, config parsing, prompt building, state."""
from __future__ import annotations

import pytest

from email_plugin.config import ConfigError, Mailbox, append_mailbox, load_config, write_initial_config
from email_plugin.context import NO_REPLY, build_followup_prompt, build_prompt, reply_text
from email_plugin.policy import (HOLD, IGNORE, WAKE, Counters, InboundEmail, Verdict, evaluate,
                                 may_send, reply_action)
from email_plugin.state import State

PASS = {"spf": "pass", "dkim": "pass", "dmarc": "gray"}
MB = Mailbox(address="claude@acs.example.com", agent="claude", inbox_id="ib1",
             allow_from=("ada@example.org", "*@team.example"), peers=("bot2",),
             may_email=("*@acs.example.com", "ada@example.org"),
             max_wakes_per_hour=2, max_wakes_per_day=3, max_agent_turns=2)
OWN = {"claude@acs.example.com": "claude", "bot2@acs.example.com": "bot2",
       "bot3@acs.example.com": "bot3"}


def mail(frm="Ada <ada@example.org>", auth=PASS, headers=None, subject="hi", **kw):
    return InboundEmail(id="e1", thread_id="t1", from_raw=frm, subject=subject,
                        text="please help", auth=auth, headers=headers or {}, **kw)


def gate(email, mb=MB, **counters):
    return evaluate(email, mb, own_addresses=OWN, counters=Counters(**counters))


def test_allowed_authenticated_sender_wakes():
    assert gate(mail()).action == WAKE
    assert gate(mail("x@team.example")).action == WAKE


def test_unknown_sender_is_held_not_dropped():
    v = gate(mail("stranger@spam.example"))
    assert v.action == HOLD and "allow_from" in v.reason


def test_forged_from_is_held_when_auth_fails():
    v = gate(mail(auth={"spf": "pass", "dkim": "fail", "dmarc": "fail"}))
    assert v.action == HOLD and "not authenticated" in v.reason


def test_dmarc_pass_alone_is_enough_and_spf_alone_is_not():
    assert gate(mail(auth={"spf": "fail", "dkim": "gray", "dmarc": "pass"})).action == WAKE
    assert gate(mail(auth={"spf": "pass", "dkim": "gray", "dmarc": "gray"})).action == HOLD


def test_require_auth_off():
    mb = Mailbox(**{**MB.__dict__, "require_auth": False})
    assert gate(mail(auth={}), mb).action == WAKE


@pytest.mark.parametrize("headers,frm", [
    ({"auto-submitted": "auto-replied"}, "ada@example.org"),
    ({"precedence": "bulk"}, "ada@example.org"),
    ({"list-id": "<news.example.org>"}, "ada@example.org"),
    ({"list-unsubscribe": "<mailto:x>"}, "ada@example.org"),
    ({}, "MAILER-DAEMON@example.org"),
    ({}, "noreply@team.example"),
])
def test_automated_mail_never_wakes(headers, frm):
    assert gate(mail(frm, headers=headers)).action == IGNORE


def test_auto_submitted_no_is_a_human():
    assert gate(mail(headers={"auto-submitted": "no"})).action == WAKE


def test_deny_beats_allow():
    mb = Mailbox(**{**MB.__dict__, "deny_from": ("ada@example.org",)})
    assert gate(mail(), mb).action == IGNORE


def test_subject_prefix():
    mb = Mailbox(**{**MB.__dict__, "subject_prefix": "[agent]"})
    assert gate(mail(subject="hello"), mb).action == HOLD
    assert gate(mail(subject="[agent] do it"), mb).action == WAKE


def test_budget_holds():
    assert gate(mail(), wakes_last_hour=2).action == HOLD
    assert gate(mail(), wakes_last_day=3).action == HOLD


def test_peer_agents_and_loop_guard():
    peer = mail("bot2@acs.example.com", headers={"auto-submitted": "auto-replied"})
    v = gate(peer)
    assert v.action == WAKE and v.peer == "bot2"  # our own Auto-Submitted doesn't block peers
    assert gate(peer, agent_turns_in_thread=2).action == HOLD
    assert gate(mail("bot3@acs.example.com")).action == IGNORE  # not a peer
    assert gate(mail("claude@acs.example.com")).action == IGNORE  # itself


def test_may_send():
    assert may_send(MB, ["bot2@acs.example.com", "Ada <ada@example.org>"]) is None
    assert "may_email" in may_send(MB, ["eve@evil.example"])
    assert may_send(MB, []) == "no recipients"


def test_prompt_frames_untrusted_content_and_thread():
    thread = [{"id": "e0", "direction": "outbound", "from": "claude@acs.example.com",
               "to": ["ada@example.org"], "subject": "x", "text": "earlier reply"},
              {"id": "e1", "direction": "inbound", "from": "ada@example.org",
               "to": ["claude@acs.example.com"], "subject": "Re: x", "text": "follow up",
               "attachments": [{"filename": "a.pdf", "size": 10}]}]
    mb = Mailbox(**{**MB.__dict__, "instructions": "Be brief."})
    p = build_prompt(mb, mail(), thread, Verdict(WAKE, "ok"))
    assert "untrusted external content" in p and "Be brief." in p
    assert "earlier reply" in p and "follow up" in p and "a.pdf" in p
    assert NO_REPLY in p and "authenticated sender" in p
    only = build_prompt(Mailbox(**{**MB.__dict__, "context": "message"}), mail(), thread,
                        Verdict(WAKE, "ok"))
    assert "earlier reply" not in only and "follow up" in only


def test_reply_action_per_sender():
    assert MB.reply_mode == "per_sender"  # D7 default
    ok = Verdict(WAKE, "ok")
    assert reply_action(mail(), MB, ok)[0] == "send"
    assert reply_action(mail("x@team.example"), MB, ok)[0] == "send"
    peer = Verdict(WAKE, "peer agent", "bot2")
    assert reply_action(mail("bot2@acs.example.com"), MB, peer) == ("send", "peer agent bot2")
    mode, why = reply_action(mail(auth={"dkim": "fail", "dmarc": "fail"}), MB, ok)
    assert mode == "draft" and "not authenticated" in why
    narrow = Mailbox(**{**MB.__dict__, "auto_send_from": ("ada@example.org",)})
    assert reply_action(mail("x@team.example"), narrow, ok)[0] == "draft"
    public = Mailbox(**{**MB.__dict__, "allow_from": ("*",)})
    assert reply_action(mail(), public, ok)[0] == "draft"  # "*" opens the gate, trusts no one
    for fixed in ("send", "draft"):
        mb = Mailbox(**{**MB.__dict__, "reply_mode": fixed})
        assert reply_action(mail("stranger@x.example", auth={}), mb, ok)[0] == fixed


def test_followup_prompt_has_only_what_the_session_has_not_seen():
    thread = [{"id": "e0", "direction": "inbound", "from": "ada@example.org", "text": "old q"},
              {"id": "r1", "direction": "outbound", "from": "claude@acs.example.com",
               "text": "old answer"},
              {"id": "e1", "direction": "inbound", "from": "ada@example.org", "text": "new q"}]
    p = build_followup_prompt(MB, mail(), thread, {"e0"}, Verdict(WAKE, "ok"))
    assert "old q" not in p and "old answer" in p and "new q" in p
    assert "untrusted external content" in p and NO_REPLY in p
    msg = Mailbox(**{**MB.__dict__, "context": "message"})
    p = build_followup_prompt(msg, mail(), thread, {"e0"}, Verdict(WAKE, "ok"))
    assert "old answer" not in p and "new q" in p


def test_prompt_truncates_long_threads():
    thread = [{"id": f"e{i}", "direction": "inbound", "from": "a@b.c", "text": "x" * 500}
              for i in range(10)]
    mb = Mailbox(**{**MB.__dict__, "max_context_chars": 1200})
    p = build_prompt(mb, mail(), thread, Verdict(WAKE, "ok"))
    assert "earlier message(s) omitted" in p and len(p) < 3000


def test_reply_text():
    assert reply_text(" hi ") == "hi"
    assert reply_text(NO_REPLY) is None and reply_text("") is None and reply_text(None) is None


def test_config_round_trip(tmp_path):
    path = tmp_path / "email.toml"
    write_initial_config(path, key_file="~/k", state_path=str(tmp_path / "s.json"))
    append_mailbox(path, Mailbox(address="Bot@Acs.Example.com", agent="bot", inbox_id="i",
                                 allow_from=("a@b.c",), peers=("x",)))
    cfg = load_config(path)
    (mb,) = cfg.mailboxes
    assert mb.address == "bot@acs.example.com" and mb.allow_from == ("a@b.c",)
    assert mb.peers == ("x",) and mb.context == "thread"
    assert mb.conversation == "session" and mb.reply_mode == "per_sender"


def test_example_config_parses_with_defaults():
    from pathlib import Path

    import email_plugin

    cfg = load_config(Path(email_plugin.__file__).parent / "example.toml")
    (mb,) = cfg.mailboxes
    assert mb == Mailbox(address=mb.address, agent=mb.agent, inbox_id=mb.inbox_id,
                         allow_from=mb.allow_from)  # every other field is the default


@pytest.mark.parametrize("extra,err", [
    ('context = "everything"', "context"),
    ('agent = "bad.name"', "invalid identity"),
    ('surprise = 1', "unknown keys"),
    ('conversation = "forever"', "conversation"),
    ('reply_mode = "maybe"', "reply_mode"),
    ('session_idle_hours = -1', "session_idle_hours"),
])
def test_config_rejects_bad_mailboxes(tmp_path, extra, err):
    path = tmp_path / "email.toml"
    body = 'address = "a@b.co"\nagent = "a"\ninbox_id = "i"\n'
    if extra.startswith("agent"):
        body = body.replace('agent = "a"\n', "")
    path.write_text(f'resend_api_key_file = "k"\nstate_path = "s"\n[[mailbox]]\n{body}{extra}\n')
    with pytest.raises(ConfigError, match=err):
        load_config(path)


def test_state_budget_window_and_persistence(tmp_path):
    now = [1000.0]
    st = State(tmp_path / "s.json", clock=lambda: now[0])
    st.record_wake("claude")
    now[0] += 4000
    st.record_wake("claude")
    assert st.wakes_within("claude", 3600) == 1 and st.wakes_within("claude", 86400) == 2
    st.mark("e1", WAKE, "ok")
    st.add_agent_turn("t1")
    st.set_thread_session("ib", "t1", "s1", "claude")
    st.session_turn("ib", "t1", done=True, seen=["e0", "e1"])
    st.session_turn("ib", "t1", done=False, seen=["e2"])  # a failed turn saw nothing
    st.save()
    again = State(tmp_path / "s.json", clock=lambda: now[0])
    assert again.is_processed("e1") and again.agent_turns("t1") == 1
    rec = again.thread_session("ib", "t1")
    assert rec["session_id"] == "s1" and rec["turns"] == 1 and rec["seen"] == ["e0", "e1"]
    assert again.drop_thread_session("ib", "t1") == rec and again.thread_sessions() == []


def test_state_from_before_sessions_loads(tmp_path):
    path = tmp_path / "old.json"
    path.write_text('{"processed": {}, "thread_counts": {}, "wakes": {}, '
                    '"agent_turns": {}, "baselined": ["ib"]}')
    st = State(path)
    assert st.baselined("ib") and st.thread_session("ib", "t1") is None
