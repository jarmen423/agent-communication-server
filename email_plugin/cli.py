"""``python -m email_plugin --config FILE <command>``.

  add-address  give an agent an address (creates the Resend inbox, appends config)
  list         show configured mailboxes and their policy
  check        dry run: what the gate would do with mail in each inbox (no wakes)
  run          the plugin: poll, gate, wake agents, reply
"""
from __future__ import annotations

import argparse
import asyncio
import logging
import os
import sys
from pathlib import Path

from .config import ConfigError, Mailbox, append_mailbox, load_config, valid_address, write_initial_config
from .policy import Counters, evaluate
from .resend_api import ResendError, ResendInboxes
from .state import DAY, State

DEFAULT_CONFIG = "~/.config/acs/email.toml"
DEFAULT_KEY = "~/.config/acs/resend-key"
DEFAULT_STATE = "~/.local/state/acs-email/state.json"


def _api(cfg) -> ResendInboxes:
    return ResendInboxes(cfg.api_key(), cfg.resend_api_url)


async def cmd_add_address(args) -> int:
    path = Path(os.path.expanduser(args.config))
    if not path.exists():
        write_initial_config(path, key_file=args.resend_key_file, state_path=DEFAULT_STATE)
        print(f"created {path}")
    cfg = load_config(path)
    address = args.address.strip().lower()
    if not valid_address(address):
        print(f"bad address {address!r}", file=sys.stderr)
        return 2
    if address in cfg.own_addresses() or cfg.mailbox_for_agent(args.agent):
        print(f"{address} or agent {args.agent} is already configured", file=sys.stderr)
        return 2
    api = _api(cfg)
    try:
        existing = {i["email_address"].lower(): i["id"] for i in await api.list_inboxes()}
        if address in existing:
            inbox_id = existing[address]
            print(f"using existing inbox {inbox_id}")
        else:
            inbox_id = await api.create_inbox(address, args.name or f"{args.agent} (agent)",
                                              args.from_name or args.agent)
            print(f"created inbox {inbox_id}")
    finally:
        await api.close()
    mb = Mailbox(address=address, agent=args.agent, inbox_id=inbox_id,
                 allow_from=tuple(a.lower() for a in args.allow_from),
                 peers=tuple(args.peer), may_email=tuple(a.lower() for a in args.may_email),
                 context=args.context, reply_mode=args.reply_mode)
    append_mailbox(path, mb)
    print(f"{address} → agent {args.agent} added to {path}")
    if not mb.allow_from and not mb.peers:
        print("note: allow_from is empty, so every email will be held until you add senders")
    return 0


def cmd_list(args) -> int:
    cfg = load_config(os.path.expanduser(args.config))
    for mb in cfg.mailboxes:
        print(f"{mb.address} → {mb.agent}  (inbox {mb.inbox_id})")
        print(f"  allow_from={list(mb.allow_from)} deny_from={list(mb.deny_from)} "
              f"require_auth={mb.require_auth}")
        print(f"  peers={list(mb.peers)} may_email={list(mb.may_email)} "
              f"max_agent_turns={mb.max_agent_turns}")
        print(f"  budget={mb.max_wakes_per_hour}/h {mb.max_wakes_per_day}/day "
              f"context={mb.context} reply_mode={mb.reply_mode}")
    return 0


async def cmd_check(args) -> int:
    from .bridge import to_inbound

    cfg = load_config(os.path.expanduser(args.config))
    state = State(cfg.state_path)
    api = _api(cfg)
    try:
        for mb in cfg.mailboxes:
            print(f"== {mb.address} → {mb.agent}")
            for t in await api.list_threads(mb.inbox_id, limit=args.limit):
                for msg in await api.list_thread_emails(mb.inbox_id, t["id"]):
                    if msg.get("direction") != "inbound":
                        continue
                    email = to_inbound(t["id"], msg, await api.received_email(msg["id"]))
                    v = evaluate(email, mb, own_addresses=cfg.own_addresses(), counters=Counters(
                        state.wakes_within(mb.agent, 3600), state.wakes_within(mb.agent, DAY),
                        state.agent_turns(t["id"])))
                    seen = state.data["processed"].get(msg["id"], {}).get("action")
                    print(f"  {v.action:6} {email.from_addr:40} {email.subject[:40]!r:44} "
                          f"{v.reason}" + (f"  [already {seen}]" if seen else ""))
    finally:
        await api.close()
    return 0


async def cmd_run(args) -> int:
    from nats_connect import connect_nats

    from .bridge import EmailBridge
    from .hub import HubLink

    cfg = load_config(os.path.expanduser(args.config))
    if not cfg.mailboxes:
        print("no mailboxes configured; use add-address first", file=sys.stderr)
        return 2
    url = args.nats_url or cfg.nats_url or os.environ.get("NATS_URL", "nats://127.0.0.1:4222")
    nc = await connect_nats(url, name=cfg.identity, identity=cfg.identity)
    api = _api(cfg)
    bridge = EmailBridge(cfg, api, HubLink(nc, cfg.identity), State(cfg.state_path))
    try:
        await bridge.run()
    finally:
        await bridge.drain()
        await api.close()
        await nc.close()
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="python -m email_plugin", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--config", default=DEFAULT_CONFIG)
    p.add_argument("-v", "--verbose", action="store_true")
    sub = p.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("add-address", help="give an agent an email address")
    a.add_argument("--agent", required=True)
    a.add_argument("--address", required=True)
    a.add_argument("--name", help="internal inbox name (Resend dashboard)")
    a.add_argument("--from-name", help="display name on sent mail (default: the agent)")
    a.add_argument("--allow-from", action="append", default=[], help="sender pattern (repeatable)")
    a.add_argument("--peer", action="append", default=[], help="agent allowed to wake this one")
    a.add_argument("--may-email", action="append", default=[], help="recipient pattern")
    a.add_argument("--context", choices=("message", "thread"), default="thread")
    a.add_argument("--reply-mode", choices=("send", "draft"), default="send")
    a.add_argument("--resend-key-file", default=DEFAULT_KEY)
    sub.add_parser("list", help="show configured mailboxes")
    c = sub.add_parser("check", help="dry run of the gate over current mail")
    c.add_argument("--limit", type=int, default=20, help="threads per mailbox")
    r = sub.add_parser("run", help="run the plugin")
    r.add_argument("--nats-url", default=None)
    args = p.parse_args(argv)
    logging.basicConfig(level=logging.DEBUG if args.verbose else logging.INFO,
                        format="%(asctime)s %(levelname)s %(name)s %(message)s")
    logging.getLogger("httpx").setLevel(logging.WARNING)
    try:
        if args.cmd == "list":
            return cmd_list(args)
        handler = {"add-address": cmd_add_address, "check": cmd_check, "run": cmd_run}[args.cmd]
        return asyncio.run(handler(args))
    except (ConfigError, ResendError, FileNotFoundError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130
