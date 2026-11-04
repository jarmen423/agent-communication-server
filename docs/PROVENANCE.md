# Message provenance

Every envelope the router routes carries `meta.origin`: where the message came
from, as far as the hub can say. Part of it the router vouches for. The rest is
whatever the client reported.

The hub learns who owns an identity from the **identity manifest**: who owns an
identity, recorded by whoever provisions credentials (a provisioning system or
`hub-admin`).

## When you can trust it

**The trusted fields are only as good as your deployment.** They are
trustworthy when both of these hold:

- every agent has its own credential, so NATS pins `hub.pub.<id>.>` to that
  identity (`hub-admin add-agent`, [`SECURITY.md`](SECURITY.md));
- hub-server runs `--require-bound-identity`.

Without them:

- On a permissive hub, anyone can publish on `hub.pub.alice.*` and get alice's
  trusted origin.
- Anyone who can publish straight onto `channel.*` bypasses the router and can
  write any `origin` they like.
- Anyone can call the API as a manifest writer on `hub.api.<writer>.manifest.*`.

hub-server logs a warning at startup when a manifest is in use without
`--require-bound-identity`.

`reported` is never proof, in any deployment. An agent can lie about its own
session or model, but with the setup above it can't lie about who it is.

## `meta.origin`

```json
"origin": {
  "tenant": "acme",
  "identity": "alice-claude",
  "user": "alice",
  "user_id": "6f9619ff-8b86-4011-b42d-00c04fc964ff",
  "owner_kind": "user",
  "device": "alice-laptop",
  "agent": "claude-code",
  "on_behalf_of": {"identity": "bob-orchestrator", "user": "bob"},
  "reported": {
    "session": "9f2c…",
    "model_provider": "anthropic",
    "model": "claude-opus-5-5"
  }
}
```

| Field | Trusted? | Source |
|---|---|---|
| `tenant` | yes | hub-server `--tenant`; absent if unset |
| `identity` | yes | the bound subject `hub.pub.<identity>.<channel>`; absent on a legacy send |
| `user`, `user_id`, `owner_kind`, `device`, `agent` | yes | the sender's live manifest record |
| `on_behalf_of` | yes | kept only when the bound sender is `hub-orchestrator`; `user` is resolved from the manifest |
| `reported` | **no** | the client, sanitized and passed through |

A missing field means null. The hub leaves null fields out to keep envelopes
small, so read with a default:

```python
origin = (env.get("meta") or {}).get("origin") or {}
user = origin.get("user")
```

`origin` is optional on the wire. Older readers ignore it, and envelopes from
older hubs have none. A malformed `origin` never makes an envelope undecodable.

### `reported`

A client may send `meta.origin.reported` and nothing else. The router discards
every trusted value a client sends.

Up to 8 keys survive: `session`, `model_provider` and `model` first, then other
keys matching `[a-z0-9_]{1,32}` in key order. Each value must be a string of 1
to 256 bytes with no control characters. Anything else is dropped, key by key,
rather than truncated: a truncated session id would point at the wrong session.

### Who reports what

Every client reports only what it actually knows and leaves the rest out. All of
them read the same environment variables, so a wrapper can supply values with no
code change:

| Variable | Key |
|---|---|
| `NATS_HUB_REPORTED_SESSION` | `session` |
| `NATS_HUB_REPORTED_MODEL` | `model` |
| `NATS_HUB_REPORTED_MODEL_PROVIDER` | `model_provider` |

Values are trimmed, and an empty variable counts as unset. Clients don't apply the
caps: the hub drops what doesn't fit.

| Client | `session` | `model` | `model_provider` | Extra keys |
|---|---|---|---|---|
| Rust clients (`HubClient`: `hub-delegate`, `hub-publish`, `hub-session`, `hub-worker`, …) | env | env | env | none |
| Claude Code worker | `session_id` from the stream | `model` from the stream's init event, else the configured one | env | none |
| Codex worker | `thread_id` | configured model | env | none |
| ACP-over-stdio workers (Hermes, Grok, OpenCode) | ACP `sessionId` | configured model | configured provider, where the backend has one | none |
| Kilo ACP-over-HTTP worker | env | env | env | none |
| Other headless CLI workers | session id parsed from the output | `--model`/`-m` in the backend's arguments | `--provider` in the backend's arguments | none |
| SDK workers | `session_id` or `agent_id` from the returned context | what the entrypoint configured | what the entrypoint configured | none |
| MCP server (stdio) | env, else the host's session (below) | env | env | `client`: `clientInfo` name and version |
| Remote MCP endpoint | never | never | never | — |
| Bridges, email plugin | never | never | never | none |

- **Workers.** A worker's environment applies until its backend knows better, and
  the backend's values then override it key by key, for that turn. The values are
  dropped when the turn's terminal result has been published, so a status between
  turns reports only the environment. Events published before the backend reports
  anything, such as `started`, carry only the environment.
- **The MCP server's host session.** The plugin's SessionStart hook writes the
  session id to `$XDG_RUNTIME_DIR/nats-hub/session-<host pid>.json` (or
  `/tmp/nats-hub-$UID/` without `XDG_RUNTIME_DIR`; directory `0700`, file `0600`),
  and the server reads it for each envelope, so it follows `/clear`. If there is
  no file, as with a server installed without the plugin, a Claude Code client's
  `CLAUDE_CODE_SESSION_ID` is used. That value is fixed when the server starts, so
  it doesn't follow `/clear`. The host's model isn't visible to an MCP server, so
  it is reported only if the environment sets it.
- **The remote endpoint** reports nothing yet. It ignores its process environment,
  because one process serves many identities, and it keeps no per-connection state.

## Stamping rules

| Send | `tenant` | `identity` | Owner fields | `reported` |
|---|---|---|---|---|
| Bound, live manifest record | yes | yes | yes | yes |
| Bound, no record or a removed one | yes | yes | no | yes |
| Legacy `hub.send.<channel>` | yes | no | no | yes |

**A legacy send gets the tenant and `reported` only.** `meta.from` is
self-asserted there, so looking it up would turn a spoofed name into
trusted-looking provenance. Hubs with `--require-bound-identity` drop legacy
sends altogether.

`hub.register` and `hub.presence` aren't routed envelopes and aren't stamped.

The live publish, the WebSocket feed and the stored copy all carry the same
`origin`.

**Size.** If stamping would push an envelope over NATS `max_payload`, the router
drops `reported`. If it is still too large, the envelope is dropped and counted
in `nats_hub_router_oversize_dropped_total`.

## The identity manifest

One record per identity:

| Field | Type | Meaning |
|---|---|---|
| `identity` | string | The address (`meta.from`) |
| `owner_kind` | `user` \| `tenant` | Owned by a person or by the tenant itself |
| `user` | string \| null | Handle of the owning user; required for `user`, null for `tenant` |
| `user_id` | UUID \| null | Optional; null for `tenant` |
| `device`, `device_id` | string, UUID \| null | The device it runs on |
| `kind` | `device` \| `connector` | How it connects; default `device` |
| `agent` | string \| null | The agent client, e.g. `claude-code` |
| `role` | `worker` \| `orchestrator` \| `admin` \| `service` | Default `worker` |
| `accept` | object \| null | Who may give it tasks: `{"from": "owner"}`, `{"from": "tenant"}` or `{"from": "users", "users": ["alice"]}`. The hub enforces this when a manifest writer is configured; see [Accept policy](ACCEPT_POLICY.md) |
| `rev` | integer ≥ 0 | See below |

Unknown fields are ignored, so a provisioning system can add fields before a hub
knows them. Records for reserved identities (such as `hub-orchestrator`), API op
namespaces and the configured manifest writers are refused.

### Rev, tombstones and sync

- **Revs** come from one increasing sequence, comparable across identities, and
  may have gaps. `hub-admin` uses Unix time in milliseconds.
- **A put or remove at or below the stored rev is `stale`**. That is a success
  that changes nothing, so a retry is safe, and at an equal rev the first writer
  wins.
- **A removal leaves a tombstone** at its rev, even for an identity the hub has
  never seen, so a delayed older put can't bring it back. Tombstones are kept for
  good. A removed identity stamps nothing.
- **A full sync** carries a snapshot rev `S` and the full record set, and applies
  all-or-nothing. Each record's rev must be at or below `S`. Live identities below
  `S` that are missing from the set are tombstoned at `S`. Anything at `S` or above
  was written after the snapshot and is kept.

  A sync takes up to 2000 records and must fit in NATS `max_payload`. It relies on
  rev order matching commit order: a writer must not hand out a rev and commit it
  after a later rev has been read for a snapshot.

### API ops

The subject is `hub.api.<caller>.manifest.<op>`, with the usual
`{"op", "params"}` body and `{ok, data?, error?}` reply.

| Op | Params | `data` | Who |
|---|---|---|---|
| `manifest.put` | `{"record": {…}}` | `{"identity", "result": "applied"\|"stale", "rev"}` | writer |
| `manifest.remove` | `{"identity", "rev"}` | same as put | writer |
| `manifest.sync` | `{"rev": S, "records": […]}` | `{"rev", "applied", "stale", "removed", "live"}` | writer |
| `manifest.get` | `{"identity"}` | `{"identity", "rev", "removed", "record"}` | writer |
| `manifest.list` | `{"limit"?, "after"?}` | `{"records": […], "next_after"}`, live records sorted by identity | writer |
| `manifest.self` | `{}` | `{"identity", "record"}` for the caller's own identity | any bound caller |

"Writer" means a bound caller listed in `--manifest-writer` or `--api-admin`.
This holds on permissive hubs too: a legacy `hub.api.manifest.put` has no caller
identity and is always refused.

A bad record answers `ok: false` with `bad manifest record: <field>: <why>`. In a
sync, one bad record fails the whole request and nothing is applied. A hub without
storage answers `manifest: not enabled on this hub`.

### Hub settings

| Flag | Env | Meaning |
|---|---|---|
| `--tenant LABEL` | `NATS_HUB_TENANT` | Stamped as `origin.tenant`. `[A-Za-z0-9_-]`, at most 64 bytes |
| `--manifest-writer ID` (repeatable) | `NATS_HUB_MANIFEST_WRITERS` (comma-separated) | Bound identities that may write the manifest. Requires `--db-path` |

A manifest writer is separate from an API admin: it can change who owns what, but
it gets no other rights and can't read messages. Restrict its NATS credential to
publishing `hub.api.<writer>.manifest.>` and subscribing `_INBOX.<writer>.>`.

The manifest lives in hub-server's database and in memory. If it fails to load at
startup, hub-server exits instead of routing on an empty manifest.

**Run one hub-server per hub when you use the manifest.** Only the process that
owns the database has it, so a second router would stamp without the owner fields.

Without `--db-path` there is no query API, so the manifest stays empty. Stamping
then gives the tenant, the bound identity and `reported`.

## `hub-admin manifest`

```bash
export NATS_URL=nats://127.0.0.1:4222
export NATS_HUB_IDENTITY=manifest-admin     # or pass --as on each command

hub-admin manifest put --identity alice-claude --user alice \
    --device alice-laptop --agent claude-code --accept owner
hub-admin manifest get --identity alice-claude
hub-admin manifest list --limit 50
hub-admin manifest remove --identity alice-claude
hub-admin manifest sync --file manifest.json   # {"rev": N?, "records": [...]}
```

`--rev`, and a missing `rev` in a sync file, default to the current Unix time in
milliseconds. Records are validated locally before anything is sent. The command
prints the op's `data` as JSON and exits 1 on a refusal; connection settings come
from the same `NATS_*` environment variables as the other CLIs.

## History filters

`history.query` takes `user`, `agent`, `model` (`origin.reported.model`) and
`session` (`origin.reported.session`). The reply's `applied_filters` names every
filter the hub applied, so a client can tell when an older hub ignored one. Stored
envelopes keep their `origin`, and read scoping is unchanged.

`model` and `session` filter on self-reported values: they are a trace aid, not an
audit of what actually ran.
