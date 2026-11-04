# Per-identity accept policy

A hub configured with `--manifest-writer` checks every direct message against
the target's identity manifest. Broadcast results on task channels are unaffected.
Only the manifest writer can change records on such a hub; an API admin can only
read them. Without a manifest writer, routing remains permissive, even when an
API admin has stored manifest records. Legacy sends cannot claim reserved hub identities,
even in permissive mode.

Unknown and removed targets refuse messages, including before the first manifest
sync. The reserved `hub-orchestrator` inbox remains available for wave plumbing.
Unknown senders own nothing. A bound sender's live manifest record determines
its user; client-supplied provenance cannot grant access.

- `owner`: the sender and target have the same non-null owning user.
- `users`: the sender's owning user is in the explicit list.
- `tenant`: the effective sender has a live record, including tenant-owned identities.

Waves use the creator named in `origin.on_behalf_of`, accepted only from the bound
`hub-orchestrator` sender. User-owned agents default to `owner`; human handles,
`owner`, and tenant-owned identities default to `tenant`.

A reply DM may bypass the policy when its `reply_to` names a delivered DM from
the recipient to the effective sender within 24 hours. The router keeps a bounded
index and falls back to stored envelopes after eviction or restart, with a
250 ms timeout and a bounded 60-second negative cache. This never
bypasses an unknown or removed target. Storage lookup failures refuse access.

## Refusals

The refused message is neither delivered nor stored. A bound sender receives a
control notice from `hub-server`, with `reply_to` naming the refused message:

```json
{"action":"rejected","reason":"accept_policy","message_id":"id","refused_sender":"sender","target":"worker","error":"worker does not accept tasks from sender"}
```

Unknown and removed targets use the same error, `worker is not a known identity
on this hub`. Legacy senders receive no inbox notice because their names are
self-asserted. For bound senders, a task-like channel (`task.<id>` or `wave.<id>.task.<id>`) also gets:

```json
{"status":"error","task_id":"id","result":null,"error":"reason","rejected":true,"refused_sender":"sender","message_id":"id"}
```

The router validates the channel before publishing. Durable notices use separate
stable deduplication IDs for inbox and terminal publications. Consumers should deduplicate by `meta.id`. Oversize notices are dropped and
counted; other notices are stored. In either case,
the refused ingress is acknowledged. `nats_hub_router_accept_refused_total` counts
refusals without identity labels.

`hub-delegate` and `hub-session create` exit **5** for router refusals. MCP delegate
tools report `refused`. Clients correlate `refused_sender` and `message_id` with their own send.
`hub-delegate` also exits 5 for a refusal from the assigned worker.

## Worker checks

Python workers and `hub-worker` fetch `manifest.self` at startup and refresh every
60 seconds. A transient error keeps the last good record; no record means no
worker check. With a record, missing stamped origin refuses execution, including
DMs delivered through the reply exemption. The worker evaluates stamped user and
owner-kind fields and publishes a rejected terminal only on a valid task channel.
Session-channel turns apply this policy alongside their session-owner checks.

Use identity-bound credentials and `--require-bound-identity` for trustworthy
provenance. NATS ACLs must prevent clients from publishing directly to delivery
subjects or impersonating reserved hub identities.

Run `make test-manifest` for manifest-writer checks in bound and durable modes.

The router gates DMs. Workers also gate every message that starts work,
including broadcast tasks and session turns after resume. Cancel and session
close do not start work and remain available after access changes.

Rejection terminals are emitted only for bound senders and include
`refused_sender` and `message_id`. Clients match both against their own send.
The orchestrator handles its private inbox notices and matching worker refusal
terminals. Worker terminals must name `hub-orchestrator` as `refused_sender`
and its dispatch id as `message_id`.

The reply exemption is multi-use within 24 hours: several replies may refer
to the same delivered DM. It does not grant access to unknown or removed targets.
Manifest writers require `--require-bound-identity`.

`manifest.self` reports `enforced: true` only when a manifest writer is
configured. Workers apply their check only then; administrative records alone
do not enable enforcement. Worker refusals identify the refused sender and
original message id. Session starts also report refusals on the session channel.
