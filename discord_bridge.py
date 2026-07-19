#!/usr/bin/env python3
"""Discord human bridge for nats-hub.

Bridges a human on Discord with agents on the nats-hub bus. Two directions:

  Human → NATS:  Discord message → envelope to inbox.<recipient-agent>
  NATS   → Human: every envelope on inbox.<identity> → Discord channel message

Run (live):
  export DISCORD_BOT_TOKEN="<bot token>"
  export DISCORD_CHANNEL_ID="123456789012345678"
  python3 discord_bridge.py \
      --identity human-bridge-discord \
      --recipient orchestrator \
      --nats-url nats://127.0.0.1:4222

Dry-run (no token, no discord.py required):
  python3 discord_bridge.py --identity human-bridge-discord --dry-run

This is the Phase 5 "thin adapter" template applied to Discord. It mirrors
`telegram_bridge.py` — only the transport import differs.

Requires `discord.py` (pip install discord.py). If `DISCORD_BOT_TOKEN` is
omitted, the bridge runs in --dry-run mode: it logs every message it *would*
send instead of calling the API, so the wiring is verifiable without
credentials.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os

# Graceful import: the bridge is a template; it should not hard-fail at import
# time if the transport SDK is absent.
try:
    import discord
    from discord import Client as DiscordClient
    _HAVE_DISCORD = True
except Exception:  # pragma: no cover - depends on environment
    DiscordClient = None  # type: ignore[misc,assignment]
    _HAVE_DISCORD = False

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
    parser = argparse.ArgumentParser(description="Discord ↔ nats-hub bridge")
    parser.add_argument("--identity", default="human-bridge-discord")
    parser.add_argument("--nats-url", default="nats://127.0.0.1:4222")
    parser.add_argument(
        "--discord-token",
        default=os.environ.get("DISCORD_BOT_TOKEN"),
        help="Discord bot token. Env: DISCORD_BOT_TOKEN",
    )
    parser.add_argument(
        "--channel-id",
        default=os.environ.get("DISCORD_CHANNEL_ID"),
        help="Discord channel id to bridge. Env: DISCORD_CHANNEL_ID",
    )
    parser.add_argument(
        "--recipient",
        default="orchestrator",
        help="Default agent inbox to deliver inbound Discord messages to",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Log intended Discord sends instead of calling the API",
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

    token = args.discord_token
    channel_id_raw = args.channel_id
    channel_id: int | None
    try:
        channel_id = int(channel_id_raw) if channel_id_raw else None
    except (TypeError, ValueError):
        channel_id = None

    dry_run = args.dry_run or not token or not _HAVE_DISCORD
    if not _HAVE_DISCORD and not dry_run:
        # Invariant: dry_run must cover the missing-SDK case. Defensive check.
        raise SystemExit(
            "discord.py is not installed. Install with `pip install discord.py` "
            "or run with --dry-run to verify wiring without the SDK."
        )

    if dry_run:
        print(
            f"[{args.identity}] dry-run mode (no Discord send). "
            f"token_present={bool(token)} discord_sdk={_HAVE_DISCORD}"
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

    # Shared state: the Discord client, once logged in, exposes channels via
    # `get_channel`. We populate it after `client.start(token)`.
    discord_client: "DiscordClient | None" = None

    # NATS → Human: forward every envelope on our inbox to the Discord channel.
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
            print(f"[{args.identity}] [dry-run] → Discord channel {channel_id}: {text[:120]}")
            return
        if channel_id is None or discord_client is None:
            print(f"[{args.identity}] no channel/client; dropping: {text[:80]}")
            return
        try:
            ch = discord_client.get_channel(channel_id)
            if ch is None:
                print(f"[{args.identity}] channel {channel_id} not found; dropping")
                return
            await ch.send(text[:2000])  # Discord 2000-char message cap
        except Exception as e:
            print(f"[{args.identity}] discord send failed: {e}")

    await nc.subscribe(inbox_subject, cb=on_nats)
    print(f"[{args.identity}] subscribed {inbox_subject} (NATS→Human)")

    # Human → NATS: on Discord message in the bridged channel → agent inbox.
    if not dry_run and token and _HAVE_DISCORD:
        intents = discord.Intents.default()
        intents.message_content = True
        client = DiscordClient(intents=intents)  # type: ignore[call-arg]

        @client.event
        async def on_ready() -> None:
            print(
                f"[{args.identity}] Discord connected as {client.user} "
                f"(bridging channel {channel_id})"
            )

        @client.event
        async def on_message(message: "discord.Message") -> None:
            # Ignore our own messages and anything outside the bridged channel.
            if message.author == client.user:
                return
            if message.channel.id != channel_id:
                return
            text = message.content or ""
            if not text:
                return
            channel = f"inbox.{args.recipient}"
            env = make_envelope(args.identity, channel, {"message": text})
            await nc.publish(f"hub.send.{channel}", env)
            print(f"[{args.identity}] Discord → {channel}: {text[:80]}")

        discord_client = client

        try:
            await client.start(token)
        finally:
            await nc.close()
    else:
        # Dry-run or no token: just keep the NATS→Human subscription alive.
        print(f"[{args.identity}] idle (no Discord polling). Ctrl-C to stop.")
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
