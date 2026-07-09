# nats-hub — Architecture Decision: DB Access Pattern

## Problem

SurrealDB with embedded RocksDB allows only **one process** to hold the LOCK
file at a time. Currently every CLI binary (`hub-wave`, `hub-session`,
`hub-stats`, etc.) calls `SurrealStorage::connect(db_path)` independently,
trying to open its own RocksDB instance.

When `hub-server` is running and holding the DB lock, all CLI tools fail with:

```
IO error: While lock file: .../LOCK: Resource temporarily unavailable
```

This blocks real usage — no wave orchestration, no session management, no
analytics queries while the router is running.

## Impact

- **hub-wave spawn** — cannot create/spawn/status waves while server runs
- **hub-session list/status** — cannot query sessions while server runs
- **hub-stats** — cannot run analytics while server runs
- **hub-thread** — cannot query threads while server runs
- **hub-agents** — cannot list agents while server runs
- **hub-history** — cannot query history while server runs

All integration tests pass because they use `connect_memory()` (in-process,
no lock contention).

## Options

### Option A: Query API over NATS (recommended)

hub-server subscribes to `hub.query.<operation>` subjects and handles DB
queries. CLI tools publish requests and wait for replies.

- ✅ Single DB owner (hub-server)
- ✅ All CLIs work while server runs
- ✅ Natural fit for the bus architecture
- ❌ Requires implementing request-reply handlers in hub-server
- ❌ Serialization overhead (JSON over NATS vs in-process calls)

### Option B: Separate reader/writer DBs

hub-server writes to RocksDB; CLIs read from a periodic snapshot or WAL
export.

- ❌ Complex to implement
- ❌ Read-only — CLIs can't create waves/sessions
- ❌ Stale reads

### Option C: SurrealDB server mode

Run SurrealDB as a separate server process (`surreal start`) instead of
embedded. Multiple clients can connect.

- ✅ Standard database pattern
- ❌ Extra process to manage
- ❌ Different from the "embedded, zero-config" design principle
- ❌ Network hop for every DB operation

## Recommendation

Option A (query API over NATS). It's the natural extension of the bus
architecture — hub-server becomes the single DB owner, and all CLI tools
become NATS clients that route queries through it. This aligns with the
product vision's "bus moves messages, DB answers questions" principle.
