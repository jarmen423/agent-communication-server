#!/usr/bin/env python3
"""Telegram human bridge for nats-hub.

Bridges a human on Telegram with agents on the nats-hub bus. Two directions:

  Human → NATS:  Telegram message → envelope to inbox.<recipient-agent>
  NATS   → Human: every envelope on inbox.<identity> → Telegram message

Run:
  python telegram_bridge.py \
      --identity human-bridge-telegram \
      --telegram-token "$TG_TOKEN" \
      --chat-id 123456789 \
      --nats-url nats://127.0.0.1:4222

This is the Phase 5 "thin adapter" template. Every bridge (SMS, Email, Slack,
Discord, Postiz) follows the same shape — only the transport import differs.

Requires `python-telegram-bot` (pip install python-telegram-bot). If the
Telegram token is omitted, the bridge still runs in --dry-run mode: it logs
every message it *would* send instead of calling the API, so the wiring is
verifiable without credentials.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os

# Graceful import: the bridge is a template; it should not hard-fail at import
# time if the transport SDK is absent.
try:
    from telegram import Update
    from telegram.ext import Application, MessageHandler, filters
    _HAVE_TELEGRAM = True
except Exception:  # pragma: no cover - depends on environment
    _HAVE_TELEGRAM = False

import nats
from nats.aio.msg import Msg

from nats_connect import connect_nats


def make_envelope(identity: str, channel: str, payload: dict) -> bytes:
    """Build a NATS envelope (same wire format as the Rust side)."""
    import uuid
    from datetime import datetime, timezone

    meta = {
        "id": str(uuid.uuid4()),
        "from": identity,
        "channel": channel,
        "to": None,
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "kind": "message",
        "reply_to": None,
    }
    return json.dumps({"meta": meta, "payload": payload}).encode()


def extract_text(payload: dict) -> str:
    """Pull a human-readable string out of an envelope payload."""
    if isinstance(payload, dict):
        for key in ("text", "message", "content", "prompt"):
            if isinstance(payload.get(key), str):
                return payload[key]
    return json.dumps(payload, ensure_ascii=False)


async def main() -> None:
    parser = argparse.ArgumentParser(description="Telegram ↔ nats-hub bridge")
    parser.add_argument("--identity", default="human-bridge-telegram")
    parser.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    parser.add_argument("--telegram-token", default=os.environ.get("TG_TOKEN"))
    parser.add_argument("--chat-id", default=os.environ.get("TG_CHAT_ID"))
    parser.add_argument(
        "--recipient",
        default="orchestrator",
        help="Default agent inbox to deliver inbound Telegram messages to",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Log intended Telegram sends instead of calling the API",
    )
    # ── Auth + TLS (NATS_* env vars are the defaults; flags win) ──────
    parser.add_argument("--token", default=None, help="NATS token. Env: NATS_TOKEN")
    parser.add_argument("--user", default=None, help="NATS username. Env: NATS_USER")
    parser.add_argument("--password", default=None, help="NATS password. Env: NATS_PASSWORD")
    parser.add_argument("--ca-file", default=None, help="CA bundle. Env: NATS_CA_FILE")
    parser.add_argument("--cert-file", default=None, help="mTLS cert. Env: NATS_CERT_FILE")
    parser.add_argument("--key-file", default=None, help="mTLS key. Env: NATS_KEY_FILE")
    parser.add_argument(
        "--credentials-file", default=None, help="NATS .creds. Env: NATS_CREDENTIALS_FILE"
    )
    parser.add_argument("--nkeys-seed", default=None, help="NATS NKEY seed. Env: NATS_NKEYS_SEED")
    parser.add_argument(
        "--tls-insecure",
        action="store_true",
        help="Disable cert verification. Refused unless NATS_ALLOW_INSECURE=1.",
    )
    args = parser.parse_args()

    token = args.telegram_token
    dry_run = args.dry_run or not token or not _HAVE_TELEGRAM
    if dry_run:
        print(
            f"[{args.identity}] dry-run mode (no Telegram send). "
            f"token_present={bool(token)} telegram_sdk={_HAVE_TELEGRAM}"
        )

    nc = await connect_nats(
        args.nats_url,
        token=args.token,
        user=args.user,
        password=args.password,
        ca_file=args.ca_file,
        cert_file=args.cert_file,
        key_file=args.key_file,
        credentials_file=args.credentials_file,
        nkeys_seed=args.nkeys_seed,
        tls_insecure=args.tls_insecure,
        name=args.identity,
    )
    inbox = f"inbox.{args.identity}"
    inbox_subject = f"channel.{inbox}"

    # NATS → Human: forward every envelope on our inbox to Telegram.
    async def on_nats(msg: Msg) -> None:
        try:
            env = json.loads(msg.data.decode())
            text = extract_text(env.get("payload", {}))
        except Exception as e:
            print(f"[{args.identity}] decode error: {e}")
            return
        if not text:
            return
        if dry_run:
            print(f"[{args.identity}] [dry-run] → Telegram chat {args.chat_id}: {text[:120]}")
            return
        if not token or not args.chat_id:
            print(f"[{args.identity}] no token/chat-id; dropping: {text[:80]}")
            return
        try:
            bot = (await Application.builder().token(token).build()).bot
            await bot.send_message(chat_id=int(args.chat_id), text=text[:4000])
        except Exception as e:
            print(f"[{args.identity}] telegram send failed: {e}")

    await nc.subscribe(inbox_subject, cb=on_nats)
    print(f"[{args.identity}] subscribed {inbox_subject} (NATS→Human)")

    # Human → NATS: poll Telegram for inbound messages → agent inbox.
    if not dry_run and token and _HAVE_TELEGRAM:
        app = Application.builder().token(token).build()

        async def on_telegram(update: "Update", _ctx) -> None:
            if not update.message or not update.message.text:
                return
            channel = f"inbox.{args.recipient}"
            env = make_envelope(args.identity, channel, {"message": update.message.text})
            await nc.publish(f"hub.send.{channel}", env)
            print(f"[{args.identity}] Telegram → {channel}: {update.message.text[:80]}")

        app.add_handler(MessageHandler(filters.TEXT & ~filters.COMMAND, on_telegram))
        print(f"[{args.identity}] polling Telegram for inbound messages…")
        await app.run_polling()
    else:
        # Dry-run or no token: just keep the NATS→Human subscription alive.
        print(f"[{args.identity}] idle (no Telegram polling). Ctrl-C to stop.")
        try:
            while True:
                await asyncio.sleep(3600)
        except asyncio.CancelledError:
            pass

    await nc.close()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        pass
