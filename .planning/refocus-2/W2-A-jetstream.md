# W2-A — JetStream durable delivery

Read `_COMMON.md` first. Contracts that still apply: `refocus.md` §6 (the reply
contract), `refocus-iteration-2.md` §4.1 (bound subjects), §4.2 (cancel), and
`HARDENING.md` C1–C6.

## Why

Today every hop is core NATS, which is fire-and-forget:

- A DM to an offline agent is lost.
- Anything published while `hub-server` restarts is lost. Clients publish to
  `hub.pub.<id>.<ch>`, which only the router subscribes to.
- A delegator that disconnects mid-task, such as a laptop going to sleep, never
  sees the result.

The product is heading toward a **hosted** service (see the memory and the
north-star notes): agents on laptops that sleep and reconnect over `wss://` to a
hub we run. Durable delivery is the main thing the hosted product needs.

## Acceptance criteria (from `refocus-iteration-2.md` §2, "Routing & delivery")

1. **A send during a router outage is not lost.** Publish while `hub-server` is
   down; it is routed after the router comes back.
2. **Offline DMs are delivered.** A DM to an agent that is offline is delivered
   when that agent reconnects, in order, within the retention window.
3. **Delivery is at-least-once with dedupe.** A redelivered envelope with the
   same `meta.id` is handled once by `HubClient`, the Python runtime and the MCP
   server.
4. **Task results survive a delegator reconnect.** Results on `channel.task.<id>`
   are retained for a bounded time. `hub-delegate --resume <task_id>` and MCP
   `wait_for_task` after a reconnect still get the terminal result.
5. **Two `hub-server`s don't double-route.** The router consumes from a shared
   durable consumer, so each envelope is routed once. Add a test that runs two
   routers.
6. **The mode is optional.** `hub-server --durable` (or config) turns on
   JetStream; without it, behaviour is exactly today's core-NATS path. CI runs
   the suite in both modes, as `make test-bound` does for identity.
7. **Permissions.** In bound mode an agent can create and read **only its own**
   inbox consumer. It cannot read another identity's stored DMs through the
   JetStream API (`$JS.API.>`). `hub-admin` generates those permissions. Add a
   dogfood check alongside `dogfood_prod_config.sh`.

## Design (a starting point; refine it and record the decisions in `docs/DELIVERY.md`)

| Stream | Subjects | Retention | Consumer |
|---|---|---|---|
| `HUB_INGRESS` | `hub.pub.*.>` (+ `hub.send.>` legacy) | work-queue, short max-age (e.g. 24 h) | one durable pull consumer `router`, shared by every hub-server (that sharing is what prevents double routing) |
| `HUB_INBOX` | `channel.inbox.>` | limits: max-age (default 7 d), max-msgs-per-subject, discard old | per-identity durable consumer `inbox-<id>`, filtered to `channel.inbox.<id>` |
| `HUB_TASKS` | `channel.task.>`, `channel.wave.>`, `channel.session.>` | limits: short max-age (default 24 h) | ephemeral or ordered consumers made on demand for resume and catch-up |

- The router **acks only after** it has republished, and set the mirror going.
  Republish with `Nats-Msg-Id = meta.id` so the stream's duplicate window
  dedupes publisher retries.
- Live subscribers keep working. Delivery subjects are unchanged, and the
  streams *capture* them; they don't replace them. Clients that want durability
  attach a consumer; others keep a plain subscription.
- Client dedupe: a bounded LRU of recently seen `meta.id`s in
  `HubClient::subscribe_inbox`, the Python runtime inbox and the MCP inbox buffer.
- **Hosted note:** each tenant will be a NATS account with its own JetStream
  limits. Keep stream names and limits configurable, avoid assumptions that
  only hold for one account, and expose per-stream limits in config.

## Write scope

Owns:
- `src/router.rs` + `src/router/**` (consume and ack)
- `src/client.rs` + `src/client/**` (durable inbox, dedupe, task resume)
- `src/connect_opts.rs`
- new `src/delivery/**` (stream and consumer setup)
- `src/bin/hub_server.rs` (flag and wiring only)
- `src/bin/hub_delegate.rs` (`--resume` only)
- `src/bin/hub_admin/**` (JetStream permissions)
- `config/*.conf*` (`jetstream {}` block)
- the inbox subscription in `worker_runtime.py` / `worker_backends/inbox.py`
- the inbox and `wait_for_task` paths in `mcp_server/` (run `sync_plugins.sh`)
- new `docs/DELIVERY.md`; `docs/SECURITY.md` and `docs/OPERATOR_HUB.md` (delivery sections)
- new `tests/delivery_*.rs`, `tests/python/test_delivery_*.py`
- new `scripts/dogfood_durable.sh`

Must NOT edit:
- `src/orchestrator/**`. Waves benefit automatically through `HUB_INGRESS` and `HUB_TASKS`.
- storage
- the visualizer
- `Makefile` / `Cargo.toml`. Propose a `make test-durable` target and any feature flags instead.

## Done means

- All seven acceptance criteria are covered by tests that run in CI, in both
  modes and under `--require-bound-identity`.
- `docs/DELIVERY.md` documents the streams, limits, failure modes and how to
  operate them. It covers what "at-least-once" means for workers: a re-run is
  possible, and the C3 dedupe covers waves.
- Under `--durable`, the router-restart and offline-DM scenarios are
  live-verified on a real stack, with the real output in the report.
