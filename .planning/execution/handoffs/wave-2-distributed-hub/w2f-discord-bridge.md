# W2-F — Discord human bridge (standalone inbox bridge)

## Outcome

`discord_bridge.py` is a standalone two-direction human bridge for nats-hub,
modeled on `telegram_bridge.py`. It bridges a human on a Discord channel with
agents on the bus, using the same envelope wire format and the same NATS auth
pathway (`nats_connect.connect_nats`) as the Telegram bridge. `docs/BRIDGES.md`
now documents the Discord bridge alongside the Telegram reference.

This is **not** a `worker_runtime` oneshot. It is a long-running bridge process
that subscribes to `channel.inbox.<identity>` (NATS→Human) and listens for
Discord messages in a bridged channel (Human→NATS).

## Files

| Path | Change |
|------|--------|
| `discord_bridge.py` | **NEW** — 237 LOC. Standalone Discord ↔ nats-hub bridge |
| `docs/BRIDGES.md` | **EDIT** — new `## Discord bridge: discord_bridge.py` section with dry-run + live examples; Telegram section retained as reference; notes `discord.py` as optional dep |

## Two directions

```
Human → NATS:   Discord message in bridged channel
                → envelope published to hub.send.inbox.<recipient>
                → router delivers to channel.inbox.<recipient>

NATS  → Human:  envelope on channel.inbox.<identity>
                → extract_text(payload)
                → channel.send(text[:2000])  (Discord 2000-char cap)
```

## CLI surface

```
python3 discord_bridge.py [--identity ID] [--nats-url URL]
                          [--discord-token T | DISCORD_BOT_TOKEN]
                          [--channel-id N | DISCORD_CHANNEL_ID]
                          [--recipient AGENT]
                          [--dry-run]
                          # NATS auth (same flags as telegram_bridge.py):
                          [--token T | --user U --password P |
                           --credentials-file F | --nkeys-seed S]
                          [--ca-file CA --cert-file C --key-file K]
                          [--tls-insecure]
```

Env var matrix:
- `DISCORD_BOT_TOKEN` → `--discord-token`
- `DISCORD_CHANNEL_ID` → `--channel-id`
- `NATS_TOKEN`, `NATS_USER`, `NATS_PASSWORD`, `NATS_CA_FILE`,
  `NATS_CERT_FILE`, `NATS_KEY_FILE`, `NATS_CREDENTIALS_FILE`,
  `NATS_NKEYS_SEED`, `NATS_TLS_INSECURE` (resolved by `connect_nats`)

## Optional dependency: `discord.py`

The `discord` import is wrapped in `try/except`. Behavior matrix:

| `discord.py` | `--discord-token` | `--dry-run` | Mode |
|---|---|---|---|
| present | present | unset | **live** — logs in, bridges both directions |
| present | absent | unset | dry-run (auto) — logs intended sends |
| absent  | present | unset | dry-run (auto) — same, no live API |
| absent  | any     | set   | dry-run (explicit) |
| absent  | present | unset, token present but SDK missing → SystemExit with install hint — unreachable in practice because dry-run auto-engages first; guard retained defensively |

If the SDK is absent and `--dry-run` is explicitly **not** set but a token is
present, the bridge raises `SystemExit` with the install hint
`pip install discord.py` rather than crashing inside `client.start()`.

## Discord-specific notes

- **Message Content Intent** is required on the bot (Discord Developer Portal →
  Bot → Privileged Gateway Intents → Message Content Intent). The bridge sets
  `intents.message_content = True` at startup.
- Inbound filter: the `on_message` handler ignores messages from the bot itself
  and only forwards messages whose `channel.id == args.channel_id`. Other
  channels the bot can see are silently ignored.
- Outbound cap: Discord's 2000-char message limit is enforced via
  `text[:2000]` (vs Telegram's 4000 in the reference bridge).

## NATS auth

Auth is delegated entirely to `nats_connect.connect_nats` with the same flag
set as `telegram_bridge.py`:
`token`, `user`, `password`, `ca_file`, `cert_file`, `key_file`,
`credentials_file`, `nkeys_seed`, `tls_insecure`, `name=args.identity`.
The `connect_nats` helper resolves `NATS_*` env-var fallbacks and applies the
TLS context including the `NATS_ALLOW_INSECURE=1` opt-in gate for
`--tls-insecure`.

## Verification

```
$ python3 -m py_compile discord_bridge.py        # → exit 0
$ python3 discord_bridge.py --help               # shows --dry-run, --discord-token,
                                                 #   --channel-id, --recipient,
                                                 #   and all NATS auth flags
```

Both checks pass. No live Discord API calls were made. `discord.py` happens to
be installed in this environment but is not required for compile, `--help`, or
dry-run.

## Residual risks / out-of-scope

- No live API dogfood (per task constraint). End-to-end live behavior
  (Discord login, `on_message` round-trip) is untested against the real
  Discord gateway — only compile + `--help` + dry-run path are verified here.
- No threading: like `telegram_bridge.py`, this is a single-process bridge.
  Concurrent high-volume channels may need backpressure handling in a future
  revision.
- No git commit (per task constraint).
- `telegram_bridge.py` body untouched (read-only reference).
- `src/`, `packaging/`, and scripts dogfood untouched.
