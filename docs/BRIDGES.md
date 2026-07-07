# Human Bridges (Phase 5)

nats-hub is a communication layer, not a chat app. Humans join the bus through
**bridge workers** — thin adapters that translate between NATS envelopes and a
human-facing transport (Telegram, SMS, Email, Slack, Discord, Postiz, …).

Every bridge is the same shape. Only the transport import differs.

## The two directions

```
Human → NATS:   human sends message → envelope published to inbox.<agent>
NATS  → Human:   envelope on inbox.<bridge> → forwarded to the human's chat
```

The bus stays agnostic: agents never know they're talking to a human. They just
see envelopes on `inbox.<identity>`.

## Template: `telegram_bridge.py`

`telegram_bridge.py` is the reference implementation. It:

1. Connects to NATS and subscribes to `channel.inbox.<identity>`.
2. On each envelope → forwards `extract_text(payload)` to the Telegram chat
   (`bot.send_message`). This is the **NATS → Human** direction.
3. Polls Telegram for inbound messages and publishes each as an envelope to
   `inbox.<recipient-agent>`. This is the **Human → NATS** direction.

### Dry-run mode (no credentials needed)

Without a `--telegram-token`, the bridge runs in `--dry-run` mode: it logs every
message it *would* send instead of calling the API. This lets you verify the
NATS wiring end-to-end without a Telegram bot token:

```bash
# terminal 1 — router
./target/debug/hub-server --db-path nats_hub.db

# terminal 2 — bridge (dry-run)
python3 telegram_bridge.py --identity human-bridge-telegram --dry-run

# terminal 3 — send a message that should reach the human
./target/debug/hub-publish --channel inbox.human-bridge-telegram \
    --to human-bridge-telegram --from agentA --message "Hello human"
# → bridge logs: [dry-run] → Telegram chat None: Hello human
```

### Live mode

```bash
export TG_TOKEN="<bot token>"
python3 telegram_bridge.py \
    --identity human-bridge-telegram \
    --chat-id 123456789 \
    --recipient orchestrator
```

## Adding another bridge (SMS, Email, Slack, …)

Copy `telegram_bridge.py` and swap the transport:

| Transport | Inbound (Human→NATS) | Outbound (NATS→Human) |
|---|---|---|
| Telegram | `python-telegram-bot` polling | `bot.send_message` |
| SMS (Twilio) | Twilio webhook → publish | Twilio REST `messages.create` |
| Email | IMAP idle → publish | SMTP `sendmail` |
| Slack | `slack-bolt` socket mode | `client.chat_postMessage` |
| Discord | `discord.py` `on_message` | `channel.send` |
| Postiz | Postiz webhook → publish | `postiz post` |

Each bridge:
- subscribes to `channel.inbox.<identity>`,
- extracts a string from the payload (`text` / `message` / `content` / `prompt`),
- forwards to the human,
- and publishes inbound human messages to `inbox.<recipient>`.

No changes to the Rust bus are required — bridges are pure adapters.
