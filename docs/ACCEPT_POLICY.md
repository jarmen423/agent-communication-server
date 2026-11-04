# Per-identity accept policy

A hub configured with `--manifest-writer` checks every direct message against
the target's identity manifest. Broadcast results on task channels are unaffected.
Without a manifest writer, routing remains permissive, even when an API admin
has stored manifest records.

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
index and falls back to stored envelopes after eviction or restart. This never
bypasses an unknown or removed target. Storage lookup failures refuse access.

## Refusals

The refused message is neither delivered nor stored. A bound sender receives a
control notice from `hub-server`, with `reply_to` naming the refused message:

```json
{"action":"rejected","reason":"accept_policy","message_id":"id","target":"worker","error":"worker does not accept tasks from sender"}
```

Unknown and removed targets use the same error, `worker is not a known identity
on this hub`. Legacy senders receive no inbox notice because their names are
self-asserted. A task-like channel (`task.<id>` or `wave.<id>.task.<id>`) also gets:

```json
{"status":"error","task_id":"id","result":null,"error":"reason","rejected":true}
```

The router validates the channel before publishing. Durable notices use separate
stable deduplication IDs for inbox and terminal publications. Core subscribers
may see the same terminal ID again; consumers should deduplicate by `meta.id`.
Notices are stored;
the refused ingress is acknowledged. `nats_hub_router_accept_refused_total` counts
refusals without identity labels.

`hub-delegate` and `hub-session create` exit **5** for router refusals. MCP delegate
tools report `refused`. Clients accept router terminals only from `hub-server`.

## Worker checks

Python workers and `hub-worker` fetch `manifest.self` at startup and refresh every
60 seconds. A transient error keeps the last good record; no record means no
worker check. With a record, missing stamped origin refuses execution, including
DMs delivered through the reply exemption. The worker evaluates stamped user and
owner-kind fields and publishes a rejected terminal only on a valid task channel.
Session-channel follow-ups retain their session-owner checks.

Use identity-bound credentials and `--require-bound-identity` for trustworthy
provenance. NATS ACLs must prevent clients from publishing directly to delivery
subjects or impersonating reserved hub identities.

Run `make test-manifest` for manifest-writer checks in bound and durable modes.
