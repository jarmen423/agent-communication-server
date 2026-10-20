# Durable delivery (JetStream)

By default every hop on the hub is core NATS, which is fire-and-forget:

- a DM to an agent that is offline is lost;
- anything sent while `hub-server` restarts is lost;
- a delegator that disconnects mid-task (for example a laptop going to
  sleep) never sees the result.

`hub-server --durable` turns on durable delivery over NATS JetStream. All
three problems go away within a retention window. **Without `--durable` the
hub behaves exactly as before**: plain core-NATS subscriptions, no streams,
no JetStream traffic from hub-server.

```bash
nats-server -c config/nats-server.conf          # JetStream on (jetstream {} block or -js)
hub-server --durable [--delivery-config delivery.json] ...
```

Clients don't need a flag. They detect a durable hub on their own (see
[Clients](#clients)).

## Streams

The streams *capture* the subjects the hub already uses. Delivery subjects
are unchanged, so live core-NATS subscribers keep working. A client that
wants durability attaches to a consumer; everything else keeps a plain
subscription.

| Stream (default name) | Subjects | Retention | Default limits | Read by |
|---|---|---|---|---|
| `HUB_INGRESS` | `hub.pub.*.>`, `hub.send.>` | work-queue (a message is deleted once the router acks it) | max-age 24 h | the durable pull consumer `router`, shared by every hub-server |
| `HUB_INBOX` | `channel.inbox.>` | limits, discard old | max-age 7 d, 10 000 msgs per inbox | one durable pull consumer `inbox-<id>` per identity |
| `HUB_TASKS` | `channel.task.>`, `channel.wave.>`, `channel.session.>` | limits, discard old, `allow_direct` | max-age 24 h, 10 000 msgs per channel | `DIRECT.GET` by sequence (catch-up/resume) |

Every stream has a 2-minute duplicate window. hub-server creates the
streams on startup, or updates them to the configured limits. So changing a
limit means editing the config and restarting hub-server. Retention policy
can't be changed on an existing stream (JetStream refuses it); delete the
stream to change it.

Broadcast channels (`channel.<name>`) are **not** stored. A broadcast is
still live-only, as before.

### Configuration

`--delivery-config <file.json>` (or env `NATS_HUB_DELIVERY_CONFIG`) accepts
any subset of these fields. Unknown keys are rejected.

```json
{
  "ingress": {"name": "HUB_INGRESS", "max_age_secs": 86400},
  "inbox":   {"name": "HUB_INBOX",   "max_age_secs": 604800, "max_msgs_per_subject": 10000,
              "max_bytes": -1, "max_msgs": -1, "max_msg_size": -1, "replicas": 1, "storage": "file"},
  "tasks":   {"name": "HUB_TASKS",   "max_age_secs": 86400,  "max_msgs_per_subject": 10000},
  "duplicate_window_secs": 120,
  "router_consumer": "router",
  "ack_wait_secs": 30,
  "router_max_deliver": -1,
  "router_max_ack_pending": 1000,
  "inbox_max_ack_pending": 256,
  "inbox_inactive_secs": 604800
}
```

`-1` means unlimited. `inbox_inactive_secs` removes an inbox consumer that
nobody has read for that long. It is recreated on the next attach and gets
whatever is still retained. Clients find the stream names via
`NATS_HUB_INBOX_STREAM` / `NATS_HUB_TASKS_STREAM` (defaults as above). The
attach reply also tells them the inbox stream. `hub-admin --inbox-stream` /
`--tasks-stream` must match the names you configure.

## How a message flows

1. An agent publishes to `hub.pub.<id>.<channel>`, exactly as today. The
   ingress stream stores it, even while no hub-server is running.
2. Each hub-server pulls from the shared `router` consumer and routes the
   envelope with the same rules as before (§4.1: the subject identity
   overwrites `meta.from`, `meta.to` picks the inbox, and so on).
3. If the destination is stored (inbox or task/session/wave), the router
   republishes it with a JetStream publish carrying
   `Nats-Msg-Id: hub-router:<from>:<meta.id>` and waits for the store ack.
   Broadcasts are published with core NATS.
4. Only then does the router queue the storage mirror and **ack** the
   ingress message. If the republish fails, the router naks the message
   and it is retried after 1 s.
5. The recipient reads its inbox consumer. Each DM is acked once it has
   been handed to the client's handler.

## Clients

`NATS_HUB_DURABLE_INBOX` picks the client mode. It is read by Rust
`HubClient::subscribe_inbox`, the Python worker runtime and the MCP server
(`hub_delivery.subscribe_inbox`):

| Value | Behaviour |
|---|---|
| unset / `auto` | Durable inbox if the hub offers one, else core |
| `0` / `off` | Always core (the old behaviour) |
| `1` / `require` | Durable or fail |

**Attach.** The client requests `hub.delivery.<id>.inbox`. hub-server
creates or updates `inbox-<id>` (pull, explicit ack, filtered to
`channel.inbox.<id>`, deliver-all) and replies with the stream and consumer
names. On a core hub nobody answers ("no responders"), so `auto` falls back
right away. If no hub-server answers but the consumer already exists (the
router is down right now), the client binds to it directly.

**Dedupe.** Each inbox (Rust, Python runtime, MCP inbox buffer, MCP task
trackers) keeps a bounded set of recently seen `meta.id`s (4096) and drops
repeats.

**Task results across a reconnect:**

- The task channel is now derived from the task envelope id:
  `task.<first 8 chars of the id>`. `hub-delegate` and MCP `delegate_async`
  both use this rule. A delegator can find the channel again from the id
  alone.
- `hub-delegate --resume <task_id> --to <worker> --from <me>` follows the
  task channel live and reads the stored channel back from the start, so a
  result published while nobody listened is still found. A resume timeout
  does not cancel the task; Ctrl-C does.
- While waiting, `hub-delegate`, MCP `wait_for_task` and `task_status`
  re-read the stored channel every 3 s. A result published during a
  connection blip is therefore not missed.
- If the MCP server's connection is replaced, running trackers on a
  durable hub are re-subscribed and caught up instead of failing. On a core
  hub they still fail with "connection lost".

## What "at-least-once" means for workers

- **A DM can be delivered more than once.** For example, a router crashes
  after it republished but before it acked; or the network drops the
  client's ack. Redeliveries carry the same `meta.id` and are dropped by
  the client dedupe set, and the router's `Nats-Msg-Id` stops the stored
  copy from being duplicated. Two *different* envelopes with the same
  intent, such as a delegator that re-sends a task with a new id, are not
  deduplicated: the worker runs it again.
- **A re-run is possible.** A worker acks a DM when the DM enters its
  queue, not when the task finishes. If the worker dies mid-task, the task
  is not redelivered: the delegator times out or resumes, and can cancel or
  re-delegate. If the worker dies before the ack lands, the task comes back
  after `ack_wait_secs` (30 s) and runs again.
- **Waves are covered by C3.** The orchestrator re-dispatches a wave task's
  `session_start` with the same session id. A worker that is still running
  that session aliases the new envelope to the running turn instead of
  starting a second run (HARDENING.md C3).
- **Ordering.** One router processes ingress in order, and an inbox
  consumer delivers in order. With several hub-servers, each pulls batches
  from the shared consumer, so two envelopes sent back to back may be
  routed by different routers and reach a recipient out of order.
  Redeliveries also arrive late.

## Several hub-servers

Every hub-server started with `--durable` pulls from the same durable
`router` consumer. JetStream hands each message to exactly one of them, so
nothing is routed twice. `tests/delivery_durable.rs::two_routers_route_each_envelope_once`
proves this with two routers and 100 broadcasts plus 100 DMs. Attach
requests are answered by one hub-server (queue group `hub-delivery`).

**Storage caveat.** Storage is embedded SurrealDB (RocksDB), and only one
process can open a given DB. With two hub-servers, each one mirrors only
the envelopes it routed into its own DB, so history, threads and stats are
split between them. The extra routers should run without `--db-path` (no
query API, no wave orchestrator) or on their own DBs. **HA routing with a
single, consistent history needs a shared storage backend** (SurrealDB in
server mode behind the `Storage` trait). That is future work. The hosted
plan for now is **one hub-server per tenant**; durable delivery is what
makes that hub-server safe to restart.

## Permissions (bound mode)

`hub-admin` always renders these grants. They are harmless when JetStream
is off.

| Who | May publish | Why |
|---|---|---|
| agent `<id>` | `hub.delivery.<id>.>` | ask hub-server for its inbox consumer |
| | `$JS.API.CONSUMER.INFO.HUB_INBOX.inbox-<id>` | bind its own consumer |
| | `$JS.API.CONSUMER.MSG.NEXT.HUB_INBOX.inbox-<id>` | pull its own DMs |
| | `$JS.ACK.HUB_INBOX.inbox-<id>.>` | ack them |
| | `$JS.API.DIRECT.GET.HUB_TASKS` | task/session/wave catch-up (no new visibility: `channel.task.>` etc. are already readable) |
| `hub-server` | `$JS.API.>`, `$JS.ACK.>` | create streams and consumers, ack ingress |

**Agents never create consumers.** An agent that could create one could
point a push consumer's `deliver_subject` at another identity's subjects.
nats-server does not check that subject against the creator's publish
permissions; we verified that on 2.15. The agent could then replay stored
envelopes into, for example, `channel.inbox.bob` or `hub.pub.bob.*`. So
hub-server creates every consumer, always as a pull consumer with a fixed
filter.

**Replay guard.** The reply subject of a JetStream API request
(`MSG.NEXT`, `DIRECT.GET`) is not permission-checked either. An agent could
aim a direct-get reply at `hub.pub.<other>.<channel>`, and the ingress
stream would capture the replayed envelope as if `<other>` had sent it. In
durable mode the router drops any ingress message that:

- has a non-ingress subject (a pull delivery keeps the stored message's
  original subject);
- carries `Nats-Stream`, `Nats-Sequence`, `Nats-Subject`,
  `Nats-Time-Stamp` or `Nats-Last-Sequence` headers (a direct/message get);
- or carries a `hub-router:` message id (anything the router stored).

`scripts/dogfood_durable.sh` check 5 runs this attack live.

`tests/delivery_permissions.rs` and `scripts/dogfood_durable.sh` check, on a
real `hub-admin render-config --durable` config, that an agent can't:

- attach as someone else;
- look up, pull from or delete another identity's consumer;
- create any consumer, including through the legacy `DURABLE.CREATE` or
  with a push `deliver_subject`;
- read `HUB_INBOX` by sequence (`STREAM.MSG.GET` / `DIRECT.GET`), or see
  its info.

## Failure modes

| What happens | Effect | Recovery |
|---|---|---|
| hub-server down | Sends pile up in `HUB_INGRESS` (24 h); live DMs and broadcasts stop | Restart. The backlog is routed in order. Broadcasts sent during the outage go out late, so live subscribers see them then |
| hub-server crashes mid-message | The message is redelivered after `ack_wait_secs`. The stored copy is deduped; live subscribers dedupe by `meta.id` | Automatic |
| Recipient offline | DMs wait in `HUB_INBOX` (7 d, 10 000 per inbox; oldest dropped first) | Delivered on reconnect, oldest first |
| Recipient offline longer than retention | Older DMs are gone; the consumer is removed after `inbox_inactive_secs` | Recreated on next attach |
| Delegator offline | The result waits in `HUB_TASKS` (24 h) | `hub-delegate --resume`, MCP `wait_for_task` |
| nats-server down | Nothing moves. Clients buffer and reconnect (async-nats/nats-py defaults) | Restart nats-server. The JetStream store on disk survives |
| JetStream disabled but `--durable` given | hub-server exits at startup ("stream setup failed") | Enable `jetstream {}` |
| Stream full (`max_bytes`) | Oldest messages are discarded (`discard: old`) | Raise the limits |

## Operating it

- **Enable:** add a `jetstream { store_dir, max_file_store, max_memory_store }`
  block to the NATS config (`hub-admin render-config --durable` emits one;
  so do `config/nats-server.conf` and the commented block in the prod
  example), then start hub-server with `--durable`.
- **Inspect:** with the `nats` CLI, as an admin or hub-server user:
  `nats stream ls`, `nats stream info HUB_INBOX`,
  `nats consumer info HUB_INBOX inbox-<id>`, `nats consumer info HUB_INGRESS router`.
  A large `num_pending` on `router` means routing is falling behind.
- **Purge one inbox:** `nats stream purge HUB_INBOX --subject channel.inbox.<id>`.
- **Back up:** stop nats-server and archive `store_dir`, alongside the
  SurrealDB backup in [`OPERATOR_HUB.md`](OPERATOR_HUB.md#backup).
- **Turn it off:** stop hub-server and start it without `--durable`.
  Clients fall back to core on their next connect. The streams stay until
  you delete them.

### Caveats

- **The first attach replays the retention window.** An identity's
  consumer starts at the oldest retained DM for that identity. If it used
  a core subscription on a durable hub (`NATS_HUB_DURABLE_INBOX=off`, or a
  client that predates this feature), the first durable attach redelivers
  up to 7 days of DMs it has already seen live. Don't mix modes for one
  identity, or purge its inbox subject first.
- **One reader per identity.** Two processes with the same identity share
  `inbox-<id>`, so each DM goes to only one of them. With core
  subscriptions both used to get every DM.
- `make up` and the dev scripts start nats-server with `-js`. Pass
  `--durable` to hub-server to try durable mode locally.

## Hosted / multi-tenant notes

The plan is one NATS **account** per tenant, each with its own JetStream
quota (`accounts { T1 { jetstream { max_file: 5G, max_streams: 3,
max_consumers: 1000 } users [...] } }`). Streams and consumers are
per-account, so every tenant gets its own `HUB_*` streams under the default
names, and one tenant can't see another's. Keep the per-stream limits in
`--delivery-config` below the account quota. The number of consumers grows
with identities: one `inbox-<id>` per agent, plus `router`. `hub-admin`
renders a single-account config today; rendering per-tenant accounts is
future work.

## Tests

- `make test-durable` runs the whole suite with `hub-server --durable`
  (`NATS_HUB_DURABLE=1`, wired through `scripts/dev/with_stack.sh`).
  `make test` and `make test-bound` still run the core path.
- `tests/delivery_durable.rs` covers router outage, offline DMs in order,
  dedupe, resume after a delegator disconnect, two routers, the core-mode
  check and the replay guard. Each test runs on a private JetStream
  nats-server, so it works in every suite mode, including bound.
- `tests/delivery_permissions.rs` covers AC7 on a rendered config.
- `tests/python/test_delivery_inbox.py` covers the Python runtime and MCP:
  offline DM, dedupe, and `wait_for_task` across a reconnect.
- `scripts/dogfood_durable.sh` runs the live end-to-end check on the
  production-shaped config.
