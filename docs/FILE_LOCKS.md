# File locks — advisory coordination over shared files

When several agents edit one repo, an agent announces **"I'm working on
these paths"**. Others are notified, can see who holds what, and can wait
until the paths are free. This is *advisory coordination*, not filesystem
locking — nothing is enforced on disk.

## Model

A lock covers:

- **scope** — a repo or project key, a free string (e.g. a git remote or
  repo path). Locks only conflict within one scope.
- **paths** — repo-relative files, or directory prefixes ending in `/`.
- **holder** — the acquiring identity. Always stamped from the caller's
  bound subject (`hub.api.<identity>.<op>`), never trusted from the
  payload. A legacy `hub.api.<op>` caller on a *permissive* hub must pass
  `holder` itself (self-asserted, like `meta.from` there).
- optional **reason** / **task_id**.
- **TTL** — default 15 min, max 24 h (`ttl_secs`, clamped `1..=86400`),
  renewable via `lock.renew`.

Two locks **conflict** when they share a scope and any of their paths
overlap — equal, or one a directory prefix of the other (`src/` overlaps
`src/main.rs` and `src`).

## Query-API ops

| op | params | notes |
|---|---|---|
| `lock.acquire` | `{scope, paths[], ttl_secs?, reason?, task_id?}` | all-or-nothing. `{granted:true, lock, channel}` or `{granted:false, conflicts:[…], channel}` — never a partial grant. The same holder may stack overlapping locks. |
| `lock.renew` | `{lock_id, ttl_secs?}` | holder only. New expiry = now + TTL (defaults to the lock's own TTL). |
| `lock.release` | `{lock_id, force?}` | holder only; an `--api-admin` may release anyone's (force). |
| `lock.list` | `{scope?, holder?, limit?}` | live (unexpired) locks. `channel` names the scope's event channel when `scope` is given. |
| `lock.get` | `{lock_id}` | one live lock, else "not found". |

Concurrent acquires of overlapping paths are serialized inside hub-server:
**exactly one wins** — verified by the concurrency test.

**Authorization.** The write gate (`write_authz.rs`) checks `holder`/lock
ownership before dispatch on an enforcing hub; the lock service re-checks
it for bound callers on every hub. An anonymous legacy caller on a hub
with `--api-admin` can't write locks at all.

## Expiry

Locks die three ways: `lock.release`, **TTL**, or **presence loss** — the
holder was seen on `hub.presence` and then goes silent for longer than the
liveness TTL (`hub-server --lock-liveness-secs`, env
`NATS_HUB_LOCK_LIVENESS_SECS`, default 90 — the same idea as wave worker
liveness, fed by the same presence subjects). A holder *never* seen on the
presence feed can't lose presence: its locks live until TTL or release.
State is persisted (`locks` table, schema v3) — locks survive a hub-server
restart, and liveness is re-seeded from the agent registry.

## Events

Every acquire, release and expiry is broadcast as `MessageKind::Event` on
`channel.lock.<scope-hash>` (`scope_hash` = FNV-1a/64 hex; the API
responses name the channel so clients never compute it):

```
{"event_type": "lock.acquired|lock.released|lock.expired",
 "data": {"lock_id", "scope", "holder", "paths", …, "reason": "ttl"|"presence"}}
```

Events are published by the hub as `hub-locks` (a reserved identity —
`hub-admin` won't mint it). `hub-admin` grants every agent
`channel.lock.>` subscribe in its generated permissions. The wait contract
is *events + periodic re-check*: `wait_for_locks` / `wait_for_files`
subscribe to the channel AND re-poll `lock.list`, so a missed event can't
hang a waiter.

## CLI

```
hub-lock acquire --scope repo --path src/foo.rs --path src/ [--ttl-secs 900] [--reason "…"] [--from me]
hub-lock renew   --lock-id <id> [--ttl-secs N]
hub-lock release --lock-id <id> [--force]
hub-lock list    --scope repo | --all [--json]
hub-lock get     --lock-id <id>
hub-lock wait    --scope repo --path src/foo.rs [--timeout-secs 300]
```

`acquire` exits 1 on conflict (prints the blocking locks); `wait` exits 0
when free, 2 on timeout. `--from` falls back to `NATS_HUB_IDENTITY`.

## MCP tools

`lock_files`, `release_files`, `renew_lock`, `list_locks` (read-only),
`wait_for_files` (read-only; blocks until free or timeout).

## Rust client

`ApiClient::lock_acquire / lock_renew / lock_release / lock_list /
lock_get`, `wait_for_locks(hub, api, scope, paths, timeout, poll)` —
`crates/client/src/locks.rs`.

## Security notes

- The holder comes only from the bound subject/caller — never a payload
  field. Only the holder renews or releases; admins can force.
- Lock channels are team-public (like task/session/wave channels): every
  agent may read every scope's lock list and events. Coordination data,
  not secrets.
- Advisory only: a malicious agent can ignore locks. Enforcement stays
  out-of-band (code review, CI).
