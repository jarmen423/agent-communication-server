# Email plugin: agents with their own email addresses

Status: **experimental plugin** (`email_plugin/`), started 2026-10-24.
The owner decided D1–D8 on 2026-10-27 (see [§7](#7-product-decisions)); the
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
public primitives every other client uses: DMs to an agent with the reply
contract (`refocus.md` §6), stateful sessions (the `hub-session` wire shapes),
and DMs to its own inbox. It has its own config,
its own state file and its own credentials. Consequences:

- Nothing in the router, storage, query API, workers or MCP server changes.
  Turning the plugin off is stopping a process.
- Workers don't know a task came from email; it is an ordinary session turn
  (or one-shot task). Any worker backend works unchanged.
- How deeply email integrates (hub sessions per thread, agent memory, a
  control-plane UI for policies, webhooks instead of polling) grows one step
  at a time, each behind a config switch. Hub sessions per thread are the
  first such step (§4.3, `conversation`).

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
5. **Wake.** Build the prompt (§4.2) and run it as a turn in the thread's hub
   session (§4.3, the default), or with `conversation = "oneshot"` as a
   one-shot task: subscribe `channel.task.<id>` first, DM `{prompt,
   task_channel}`, wait for the terminal `done`/`error`.
6. **Reply.** `done` → the result becomes a reply in the same thread, from the
   agent's address. With `reply_mode = "per_sender"` (the default, D7) it is
   **sent** when the sender is authenticated and trusted (a peer agent, or a
   match in `auto_send_from`; when that is empty, `allow_from` without a bare
   `*`), and saved as a **Resend draft** for a human to approve otherwise.
   `reply_mode = "send"` / `"draft"` do the same for every sender. Sent replies
   carry `Auto-Submitted: auto-replied` so vacation responders don't answer
   them (RFC 3834). A result of exactly `NO_REPLY` sends nothing. `error` or
   timeout → no email goes out (a timed-out turn is cancelled, §4.2) and the
   thread is labelled `acs-failed`.

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

With a thread session (§4.3), the first turn gets this prompt; later turns
get the same framing but only the thread messages the session hasn't been
shown yet (with `context = "message"`, only the new email), since the worker
already has the rest in memory.

Context beyond the thread stays out (§7, D4).

### 4.3 One hub session per thread (D3)

`conversation = "session"` (default) maps each email thread to one hub
session with the mailbox's agent, so the worker keeps its memory between
emails. The state file holds `inbox/thread → {session_id, agent, turns,
seen, last_turn}` (`email_plugin/sessions.py`, `state.py`).

- **First email in a thread** (that the gate lets through): the plugin picks a
  session id, writes it to the state file, creates the session record
  (`session.create`, orchestrator = the plugin identity, so a worker can
  persist and later resume its backend context), and DMs the worker
  `session_start` with the §4.2 prompt on `session.<id>`. The result arrives on
  `channel.session.<id>` under the reply contract.
- **Later emails in the thread**: `session_send` with only what the session
  hasn't seen. Turns are DMs to the worker (`meta.to`, channel `session.<id>`),
  not broadcasts on the session channel, so a worker that restarted gets the
  turn from its inbox and either resumes the session or answers `unknown
  session`. Turns in one thread never overlap (a per-thread lock).
- **A session that is gone** (the worker answers `unknown session`): the
  plugin forgets it and starts a fresh session for the same email with the
  thread text as context, as a one-shot task would. One email still gets one
  reply.
- **A session that never completed a turn, or whose last turn timed out**:
  closed and forgotten; the next email starts fresh. A failed (`error`) turn
  in an established session keeps the session; that email stays "unseen", so
  the next turn includes it.
- **Idle sessions** (`session_idle_hours`, default 168, 0 = never) are closed
  at the start of a poll (`session_close` DM + `session.update_status
  closed`); the next email in that thread starts fresh. So are sessions whose
  mailbox was removed from the config, and a session is replaced when its
  mailbox now points at another agent.
- The gate, caps and labels in front are unchanged: a session turn is a wake
  like any other.

`conversation = "oneshot"` keeps one task per email with the thread as
context. Use it for workers without session memory (the Rust `hub-worker
--execute` runs a command per prompt) or where a cold start per email is
preferable.

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
conversation = "session"      # one hub session per thread (D3); "oneshot"
reply_mode = "per_sender"     # send to allowlisted senders, draft otherwise (D7)
max_wakes_per_hour = 10
```

## 6. Verified so far

- Unit tests: `tests/python/test_email_policy.py` (every gate rule, the
  per-sender reply decision, config, prompts, state) and
  `tests/python/test_email_bridge.py` (baseline, at-most-once wakes across
  restarts, spam costs no tokens, budget, `NO_REPLY`, failures, draft mode,
  `may_email`, the Resend client's paths/retries/errors).
  `tests/python/test_email_sessions.py` covers thread sessions (new thread,
  follow-up, plugin restart, dead-session recovery, failed and timed-out
  turns, idle close, `oneshot`), per-sender replies, and the session wire
  shapes `HubLink` publishes.
- Live: `test_live_email_wakes_echo_worker_and_replies` runs the reply contract
  through a real router and `echo_worker.py`: a first email starts a thread
  session, a follow-up is a turn in it, and after a worker restart the next
  email still gets its reply. It runs in all three stack modes (default,
  `NATS_HUB_REQUIRE_BOUND=1`, `NATS_HUB_DURABLE=1`).
- Real Resend, 2026-10-24: with `echo_worker.py --identity claude` on a
  throwaway stack, an email from an allowlisted sender to
  `claude@acs.agentmemorylabs.com` woke the agent and got a delivered reply in
  the same thread (labels `acs-woke`, `acs-replied`); an email from a sender
  not on the list was labelled `acs-held` and woke nothing.

## 7. Product decisions

Decided by the owner on 2026-10-27.

| # | Decision | Options that were open | Decision (2026-10-27) | Status |
|---|---|---|---|---|
| D1 | **Who may wake an agent by email?** | explicit allowlist · any ACS user of the tenant · anyone, with a human approving the first email from a new sender · anyone (public support agent) | **Allowlist + authenticated sender only.** Everything else is held, visible in the inbox. | Built (the default) |
| D2 | **Addresses at scale** | one Resend inbox per agent (plan limit on inboxes) · one inbox per tenant routed by local part · domain-wide receiving + webhook, no Inboxes · per-tenant subdomains (`claude@tenant.acs…`) · plus-addressing (`agent+tenant@`) | **Long term: one Resend inbox per tenant, routed to agents by local part.** The plugin keeps one inbox per agent until then. | Recorded, not built |
| D3 | **Email thread ↔ hub conversation** | one-shot task per email (with thread text as context) · one hub *session* per thread (the worker keeps memory between emails) | **One hub session per thread** (§4.3); `conversation = "oneshot"` per mailbox as a fallback. | Built (the default) |
| D4 | **Context beyond the thread** | none · the agent's recent hub history / sessions / waves · agent memory (agent-memory MCP) · per-mailbox choice | **Keep the default:** none. | Built (the default) |
| D5 | **What a wake costs and who pays** | per-mailbox hourly/daily caps · tenant-wide token budget · cheap pre-classifier model before the real agent | **Keep the default:** per-mailbox caps; no classifier. | Built (the default) |
| D6 | **Where it runs** | as this plugin next to a hub · inside the hosted control plane (acs-private), which already holds the Resend key and has a public URL for webhooks | **Long term: inside the hosted control plane (acs-private).** It stays a plugin until then. | Recorded, not built |
| D7 | **Autonomy of replies** | send automatically · save as a Resend draft for a human to approve (Resend's own inbox agent only drafts) · per-sender | **Per sender:** send to allowlisted (authenticated) senders, draft for everyone else. `reply_mode = "per_sender"` is the default; `"send"` / `"draft"` stay as explicit per-mailbox overrides. | Built (the default) |
| D8 | **Attachments** | ignore · pass metadata · download into the agent's workspace | **Keep the default:** metadata only (names and sizes in the prompt). | Built (the default) |

## 8. Not built yet (in rough order)

- Move the feature into the hosted control plane (acs-private) (D6), with
  webhook ingestion (`inbox.email.received`, signed) instead of polling, since
  it has a public URL there.
- One Resend inbox per tenant, routed to agents by local part (D2), instead of
  one inbox per agent.
- A dedicated MCP tool (`send_email`) so agents don't hand-write the JSON in §4.1.
- A UI for the policy (control plane), backed by the same config fields.
- Attachments into the agent's workspace (D8 keeps metadata only for now).
