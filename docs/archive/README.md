# docs/archive — historical plans and handoffs

Everything here is **finished work, kept for its reasoning**. None of it is a
guide to the current code. The files are unedited, so they contain stale build
instructions: machine-specific paths such as `/data/cargo-targets/jfrie/nats`
and `/home/jfrie/nats`, the old test count of 39, and links to where files used
to live. For how to build and run today, see [`CONTRIBUTING.md`](../../CONTRIBUTING.md).
For how the system works today, see [`AGENTS.md`](../../AGENTS.md) and the living docs in
[`docs/`](../).

Moved here in refocus iteration 2 (T5). `git log --follow <file>` shows each
file's history before the move.

## Design plans

| File | What it was | Outcome | Current home of the topic |
|---|---|---|---|
| [`PHASE3_PLAN.md`](PHASE3_PLAN.md) | Phase 3: stateful sessions, progress events, wave orchestration | Shipped (3a/3b/3c) | `hub-session`, `hub-watch`, `hub-wave`; [`docs/WAVES.md`](../WAVES.md) once iteration 2 lands |
| [`PHASE4_PLAN.md`](PHASE4_PLAN.md) | Phase 4: `Analytics` trait (`hub-stats`) and live `MetricsCollector` (`/metrics`) | Shipped (4a/4b) | `src/analytics/` |
| [`TUI_PLAN.md`](TUI_PLAN.md) | `hub-tui` ratatui dashboard design (phases 1–6) | v1 shipped (phases 1–4) | `src/tui/`, `src/bin/hub_tui.rs` |
| [`TUI_HARDENING_HANDOFF.md`](TUI_HARDENING_HANDOFF.md) | Follow-up polish/hardening pass on `hub-tui` | Closed | `src/tui/` |
| [`DB_ACCESS_PROBLEM.md`](DB_ACCESS_PROBLEM.md) | ADR: how CLIs reach the DB while `hub-server` holds the RocksDB lock | Resolved: query API over NATS (`hub.api.>`) | `src/query_api.rs`, `src/query_api_client.rs` |

## Distributed-hub execution (waves 1–3, July 2026)

Formerly `.planning/execution/`. Status: **COMPLETE**.

| File | What |
|---|---|
| [`execution/ROADMAP.md`](execution/ROADMAP.md) | Wave/commit table for the distributed-hub readiness work |
| [`execution/tasks.json`](execution/tasks.json) | Machine-readable task registry for those waves |
| [`execution/handoffs/wave-1-remote-connectivity/`](execution/handoffs/wave-1-remote-connectivity/) | W1-B OpenCode ACP, W1-C Kilo ACP, W1-D NATS WebSocket TLS/auth (superseded by W2-A/W2-B) |
| [`execution/handoffs/wave-2-distributed-hub/`](execution/handoffs/wave-2-distributed-hub/) | W2-A Python auth/TLS, W2-B Rust auth, W2-C deploy docs, W2-D remote install DX, W2-E token dogfood, W2-F Discord bridge, W3 close-out |

The living docs that came out of these waves are [`SECURITY.md`](../SECURITY.md),
[`OPERATOR_HUB.md`](../OPERATOR_HUB.md), [`JOIN_HUB.md`](../JOIN_HUB.md),
[`REMOTE_INSTALL.md`](../REMOTE_INSTALL.md) and [`REMOTE_AGENTS.md`](../REMOTE_AGENTS.md).

## Operator scratch notes

Formerly `docs/operator-notes/`: personal command snippets, not maintained.

| File | What |
|---|---|
| [`operator-notes/hub-tui.md`](operator-notes/hub-tui.md) | Running `hub-tui` against a local stack. Today: `make up`, then `target/debug/hub-tui` |
| [`operator-notes/ht-simulate-ui.md`](operator-notes/ht-simulate-ui.md) | Faking agents with echo workers for the visualizer. Today: `make up` (starts `echo-1`/`echo-2`) |
| [`operator-notes/notes.md`](operator-notes/notes.md) | Installing the Codex/Claude plugins from the local marketplace |
