# Execution roadmap — distributed hub readiness

## Prior wave (done — docs/config only)

**wave-1-remote-connectivity:** ACP HTTP, OpenCode/Kilo ACP, NATS WS TLS/auth *config comments*.  
Gap left by W1-D: clients still cannot pass token/TLS on the CLI.

## wave-2 — Distributed hub “six pieces”

**Goal:** One hub VPS + many client machines can join safely and talk through it.

| # | Piece | Task | Wave |
|---|--------|------|------|
| 1 | Python auth/TLS connect path | W2-A | wave-2a |
| 2 | Prod NATS conf + systemd + operator docs | W2-C | wave-2a |
| 3 | Live dogfood (token + WS round-trip script) | W2-E | wave-2b |
| 4 | Rust `HubClient` auth (env + opts) | W2-B | wave-2a |
| 5 | Remote install DX | W2-D | wave-2b |
| 6 | Human bridge expansion (Discord; TUI stays plan-only) | W2-F | wave-2b |

**Explicitly out of wave-2:** full `hub-tui` (see `docs/TUI_PLAN.md`) — multi-phase, not required for distributed messaging.

### Write scope (collision-safe)

| Task | Owned paths | Must not touch |
|------|-------------|----------------|
| **W2-A** | `nats_connect.py` (NEW), `worker_runtime.py`, `remote_agent_adapter.py`, `telegram_bridge.py`, `worker_supervisor.py`, `docs/REMOTE_AGENTS.md` (client flag section only) | Rust, deploy/, discord |
| **W2-B** | `src/client.rs`, optional `tests/hub_connect_opts.rs` (NEW), `docs/SECURITY.md` section *Rust env vars* if file exists else skip | Python, deploy |
| **W2-C** | `deploy/systemd/*` (NEW), `config/nats-server.prod.conf.example` (NEW), `docs/SECURITY.md` (NEW), `docs/JOIN_HUB.md` (NEW), `docs/OPERATOR_HUB.md` (NEW) | Python connect code, src/ |
| **W2-D** | `packaging/remote/*` (NEW), `requirements-remote.txt` (NEW), `docs/REMOTE_INSTALL.md` (NEW) | core runtime logic (may *document* flags from W2-A) |
| **W2-E** | `scripts/dogfood_remote_ws.sh` (NEW), `scripts/dogfood_token_auth.sh` (NEW), handoff with run evidence | product code except tiny fixture conf under `/tmp` |
| **W2-F** | `discord_bridge.py` (NEW), `docs/BRIDGES.md` (EDIT) | telegram_bridge.py body (reference only) |

### Dependencies

```
wave-2a (parallel):  W2-A ║ W2-B ║ W2-C
        │
        ▼  parent gate: syntax + cargo test + conf validate
wave-2b (parallel):  W2-D ║ W2-E ║ W2-F
        │                (W2-E may use W2-A flags; W2-D docs after 2a)
        ▼  parent gate: dogfood script green + handoffs
```

### Parent merge gates

**After wave-2a:**

```bash
export CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats
python3 -m py_compile nats_connect.py worker_runtime.py remote_agent_adapter.py telegram_bridge.py worker_supervisor.py
nats-server -t -c config/nats-server.conf
nats-server -t -c config/nats-server.prod.conf.example   # if example is valid standalone
cargo test
cargo fmt --check
```

**After wave-2b:**

```bash
bash scripts/dogfood_token_auth.sh   # local token + WS + echo remote adapter
python3 -m py_compile discord_bridge.py
test -f docs/REMOTE_INSTALL.md && test -f packaging/remote/README.md
```

### Handoffs

`.planning/execution/handoffs/wave-2-distributed-hub/<task_id>.md`

## Wave 2 gate (parent)

- wave-2a committed: `cce1b7a`
- wave-2b parent dogfood: `bash scripts/dogfood_token_auth.sh` → **PASS** (neg control Authorization Violation + ping-wave2e echo)
- Full hub-tui still deferred to `docs/TUI_PLAN.md`
- Pre-existing test flakes (unrelated): `test_agent_activity`, `test_list_pending_storage`
