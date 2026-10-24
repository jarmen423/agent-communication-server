# Email plugin: agents with their own email addresses

Status: **experimental plugin** (`email_plugin/`), started 2026-10-24.
Product decisions are still open (see [§7](#7-open-product-decisions)); the
code is built so they can change without touching the hub's other primitives.

## 1. Why

ACS already lets agents message each other and lets humans reach them through
bridges (Telegram, Discord). Email is the one channel every person and every
company already has. If an agent has an address, **any email client can send
it work**, and the agent can reply in the same thread. No app to install, no
account on our side for the sender.

[Resend Inboxes](https://resend.com/docs/dashboard/inboxes/introduction.md)
make this cheap to build: Resend hosts the mailbox (threads, folders, labels,
drafts, sent mail) on a domain we verify, and exposes it over an API. We only
write the part that decides **whether an email should reach an agent** and
moves it between the inbox and the hub.

First address: `claude@acs.agentmemorylabs.com` (domain verified for sending
and receiving on 2026-10-24; MX `acs` → `receive.forge.rmta.net`, set up by
Resend's Cloudflare auto-configure).

## 2. What the best product version looks like

The owner's goals (2026-10-24), in their words, made concrete:

1. **Add an email address to any agent, in one step.** `add-address --agent X`
   creates the inbox and wires it to the agent.
2. **Control what wakes an agent.** An agent must never spend tokens on spam,
   newsletters, auto-replies or strangers. Every email passes a *gate* that
   costs no tokens before anything is delegated.
3. **Control which agents email each other.** Agent-to-agent email is opt-in
   on both sides, with loop protection.
4. **Control what context an agent has when it handles email.** From "this
   email only" up to "the whole thread", and later "everything else this agent
   is working on".

The plugin's answer to each, today: (1) `add-address`; (2) the gate in §4,
configured per mailbox; (3) `peers`, `may_email` and `max_agent_turns`
(§4.1); (4) `context` and `instructions` (§4.2).

## 3. Why a plugin, not a core feature

The plugin is a separate process that talks to the hub **only** through the
public primitives every other client uses: a DM to an agent with the reply
contract (`refocus.md` §6), and DMs to its own inbox. It has its own config,
its own state file and its own credentials. Consequences:

- Nothing in the router, storage, query API, workers or MCP server changes.
  Turning the plugin off is stopping a process.
- Workers don't know a task came from email; it is an ordinary task. Any
  worker backend works unchanged.
- How deeply email integrates (hub sessions per thread, agent memory, a
  control-plane UI for policies, webhooks instead of polling) can grow one
  step at a time, each behind a config switch.

It follows the bridge shape in [`BRIDGES.md`](BRIDGES.md), with a gate in the
middle.

## 4. How it works

```
             Resend Inbox (claude@acs…)                     nats-hub
 sender ──▶  thread / email  ──poll──▶  email_plugin  ──DM task──▶ agent "claude"
                                          │   gate   ◀──result────┘ (reply contract)
 sender ◀──  reply in thread  ◀──reply────┘
```

1. **Poll.** For each configured mailbox, list threads in the `inbox` folder
   and fetch the emails of threads whose message count changed. (Webhooks are
   a later option; polling needs no public endpoint.)
2. **Enrich.** For each new inbound email, fetch the received-email record:
   its **SPF/DKIM/DMARC results** (computed by Resend's receiving server, so
   the sender cannot forge them) and its headers.
3. **Gate** (pure function, no tokens; `email_plugin/policy.py`). The first
   matching rule wins:

   | Rule | Result |
   |---|---|
   | already processed (state file) | skip |
   | sent by one of our own mailboxes and not an allowed peer (§4.1) | ignore |
   | `Auto-Submitted`, `Precedence: bulk/list/junk`, `List-Id`, `List-Unsubscribe`, `mailer-daemon`/`noreply` senders | ignore |
   | `require_auth` and neither DKIM nor DMARC pass | hold (possible spoof) |
   | sender matches `deny_from` | ignore |
   | sender doesn't match `allow_from` | hold |
   | `subject_prefix` set and missing | hold |
   | agent-to-agent turns in this thread ≥ `max_agent_turns` | hold (loop guard) |
   | mailbox over `max_wakes_per_hour` / `max_wakes_per_day` | hold (budget) |
   | otherwise | **wake** |

   DKIM `pass` in Resend's result already means *signed by the From domain*,
   so "DKIM or DMARC pass" is what makes an `allow_from` match trustworthy.
   SPF alone only vouches for the envelope sender, not the `From` a human
   sees, so it is not enough.

4. **Label.** Every decision is visible in the Resend dashboard as a thread
   label: `acs-woke`, `acs-held`, `acs-ignored`. Held mail stays in the inbox
   for a human to read; nothing is deleted.
5. **Wake.** Build the prompt (§4.2) and delegate to the agent with the reply
   contract: subscribe `channel.task.<id>` first, DM `{prompt, task_channel}`,
   wait for the terminal `done`/`error`.
6. **Reply.** `done` → the result is sent as a reply in the same thread, from
   the agent's address, with `Auto-Submitted: auto-replied` so vacation
   responders don't answer it (RFC 3834). With `reply_mode = "draft"` it is
   saved as a reply draft for a human to send instead. A result of exactly
   `NO_REPLY` sends nothing. `error` or timeout → no email goes out (a timed-out
   task is cancelled, §4.2) and the thread is labelled `acs-failed`.

Each email is marked processed **before** its agent is woken: a crash can
lose a reply, but never wakes an agent twice for one email. The first time
the plugin sees an inbox it records the mail already there as `baseline`, so
starting it never replays history into an agent.

### 4.1 Agent-to-agent email

- An agent sends email by DMing the plugin identity (`email-bridge` by default)
  with `{"email": {"to": [...], "subject": "...", "text": "..."}}`. The plugin
  sends it from **that agent's** mailbox (the sender is `meta.from`, which the
  router pins to credentials on a bound hub), only if every recipient matches
  the mailbox's `may_email` list. It answers with a DM `{status, result|error}`.
  The same object as JSON in `payload.message` also works, which is what the
  MCP tool sends: `send_direct(to="email-bridge", message='{"email": {"to":
  ["bot2@acs.agentmemorylabs.com"], "subject": "…", "text": "…"}}')`.
- Mail between two of our mailboxes is real email: it arrives in the
  recipient's inbox and goes through the recipient's gate. It wakes the
  recipient only if the sending agent is in the recipient's `peers` list.
- `max_agent_turns` caps how many times the plugin wakes an agent for mail
  from peer agents in one thread, so two agents can't email each other forever.

### 4.2 Context

`context` per mailbox:

| Mode | The agent sees |
|---|---|
| `message` | the new email only |
| `thread` (default) | the whole thread, oldest first, truncated to `max_context_chars` |

Every prompt frames the email as **untrusted external content**, says who sent
it and whether the sender was authenticated, and says that the agent's final
answer becomes the reply body. Mailbox `instructions` (free text) are
prepended, e.g. "You are the support agent for X; never share credentials."

Deeper context is a product decision (§7, D4), not built yet.

## 5. Using it

Run from the repo root (the plugin imports the shared `nats_connect.py` and
`hub_delivery.py`). It needs a **full-access** Resend key: it reads inboxes and
received-email records, not just sends.

```bash
# 1. Give an agent an address (creates the Resend inbox, writes the config):
.venv/bin/python -m email_plugin --config ~/.config/acs/email.toml \
    add-address --agent claude --address claude@acs.agentmemorylabs.com \
    --allow-from you@example.com

# 2. Run the plugin next to the hub (NATS_* env for auth, like every client):
.venv/bin/python -m email_plugin --config ~/.config/acs/email.toml run

# 3. See what the gate would do with what's in the inbox, without waking anyone:
.venv/bin/python -m email_plugin --config ~/.config/acs/email.toml check
```

Config (`email_plugin/example.toml` has every field):

```toml
identity = "email-bridge"
resend_api_key_file = "~/.config/acs/resend-key"
state_path = "~/.local/state/acs-email/state.json"
poll_seconds = 15

[[mailbox]]
address = "claude@acs.agentmemorylabs.com"
agent = "claude"
inbox_id = "39757807-63f3-4a8b-b096-8a61474f0b97"
allow_from = ["you@example.com", "*@agentmemorylabs.com"]
context = "thread"
max_wakes_per_hour = 10
```

## 6. Verified so far

- Unit tests: `tests/python/test_email_policy.py` (every gate rule, config,
  prompt, state) and `tests/python/test_email_bridge.py` (baseline, at-most-once
  wakes across restarts, spam costs no tokens, budget, `NO_REPLY`, failures,
  draft mode, `may_email`, the Resend client's paths/retries/errors).
- Live: the same file's `test_live_email_wakes_echo_worker_and_replies` runs the
  reply contract through a real router and `echo_worker.py` in all three stack
  modes (default, `NATS_HUB_REQUIRE_BOUND=1`, `NATS_HUB_DURABLE=1`).
- Real Resend, 2026-10-24: with `echo_worker.py --identity claude` on a
  throwaway stack, an email from an allowlisted sender to
  `claude@acs.agentmemorylabs.com` woke the agent and got a delivered reply in
  the same thread (labels `acs-woke`, `acs-replied`); an email from a sender
  not on the list was labelled `acs-held` and woke nothing.

## 7. Open product decisions

| # | Decision | Options | Plugin default today |
|---|---|---|---|
| D1 | **Who may wake an agent by email?** | explicit allowlist · any ACS user of the tenant · anyone, with a human approving the first email from a new sender · anyone (public support agent) | allowlist + authenticated sender; everything else held, visible in the inbox |
| D2 | **Addresses at scale** | one Resend inbox per agent (plan limit on inboxes) · one inbox per tenant routed by local part · domain-wide receiving + webhook, no Inboxes · per-tenant subdomains (`claude@tenant.acs…`) · plus-addressing (`agent+tenant@`) | one inbox per agent |
| D3 | **Email thread ↔ hub conversation** | one-shot task per email (with thread text as context) · one hub *session* per thread (the worker keeps memory between emails) | one-shot task, `thread` context |
| D4 | **Context beyond the thread** | none · the agent's recent hub history / sessions / waves · agent memory (agent-memory MCP) · per-mailbox choice | none |
| D5 | **What a wake costs and who pays** | per-mailbox hourly/daily caps · tenant-wide token budget · cheap pre-classifier model before the real agent | per-mailbox caps; no classifier |
| D6 | **Where it runs** | as this plugin next to a hub · inside the hosted control plane (acs-private), which already holds the Resend key and has a public URL for webhooks | plugin |
| D7 | **Autonomy of replies** | send automatically · save as a Resend draft for a human to approve (Resend's own inbox agent only drafts) · per-sender | `reply_mode = "send"`; `"draft"` is built and per mailbox |
| D8 | **Attachments** | ignore · pass metadata · download into the agent's workspace | metadata only (names and sizes in the prompt) |

## 8. Not built yet (in rough order)

- Webhook ingestion (`inbox.email.received`, signed) instead of polling, once it
  runs somewhere with a public URL (D6).
- Thread ↔ hub session mapping (D3).
- A dedicated MCP tool (`send_email`) so agents don't hand-write the JSON in §4.1.
- A UI for the policy (control plane), backed by the same config fields.
- Attachments into the agent's workspace (D8).
