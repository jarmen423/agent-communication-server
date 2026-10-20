# nats-hub

**A message hub for your coding agents: Claude Code, Codex, Cursor, Hermes and
others, on one machine or many.** An orchestrator (you in a terminal, or an
agent through the MCP plugin) delegates a task to a worker anywhere on the hub
and gets a structured result back. You can watch the whole conversation live
and query it later.

nats-hub is one `nats-server`, one Rust router (`hub-server`), and thin
clients: CLIs, Python workers, an MCP server, a terminal dashboard and a
browser visualizer. Workers on other machines dial in over `wss://` with a
token or credentials. They don't run a server of their own.

- **Delegate and get an answer.** `hub-delegate --to codex-1 --prompt "…"`
  opens an isolated task channel, streams progress events, and returns
  `status: done | error` under one reply contract.
- **Real agent workers.** Claude Code, Codex, Cursor, Hermes, Grok, Kilo and
  OpenCode backends, plus `hub-worker --execute <any CLI>`.
- **Multi-turn and parallel.** Stateful sessions (`hub-session`) and waves of
  parallel tasks with dependencies and merge gates (`hub-wave`).
- **Orchestrate from an agent.** A Claude Code / Codex / Hermes plugin exposes
  the hub as MCP tools: `list_agents`, `delegate_async`, `wait_for_task`,
  sessions, waves and history.
- **Observable.** Every envelope is mirrored to an embedded SurrealDB. You get
  `hub-history`, `hub-thread`, `hub-stats`, Prometheus `/metrics`, the
  `hub-tui` dashboard and a browser visualizer.
- **Embeddable.** It's a Rust crate (`nats_hub`). With `no-storage` it's pure
  transport, with no database.

```
 orchestrator ──hub.send.<ch>──▶ hub-server (router) ──channel.inbox.<worker>──▶ worker
 (CLI / MCP)  ◀──channel.task.<id>── result + progress events ◀────────────────┘
                                   │
                                   └─ async mirror ─▶ SurrealDB (history, registry, sessions, waves)
```

## Quick start (5 minutes)

### A. Run a hub locally, from source

Needs Rust, a C/C++ toolchain with libclang, and Python ≥ 3.10.
`make doctor` tells you what's missing. The **first** build compiles RocksDB and
takes about 10 minutes; after that, everything below takes seconds.

```bash
git clone https://github.com/jarmen423/agent-communication-server.git nats-hub
cd nats-hub
make setup   # pinned nats-server → .tools/bin, Python venv → .venv
make up      # nats-server + hub-server + visualizer (http://127.0.0.1:9191/) + echo workers
```

In a second terminal:

```bash
./target/debug/hub-agents                                               # who's online
./target/debug/hub-delegate --to echo-1 --prompt "hello" --verbose      # → "echo: olleh"
./target/debug/hub-history --tail                                       # live history
```

Swap the echo worker for a real agent. The agent's CLI must be installed and
logged in:

```bash
.venv/bin/python claude_worker.py --identity claude-1     # Claude Code
.venv/bin/python codex_worker.py  --identity codex-1      # Codex
./target/debug/hub-delegate --to claude-1 --prompt "Summarize README.md" --verbose
```

### B. Join an existing hub from another machine (no Rust needed)

```bash
curl -fsSL https://raw.githubusercontent.com/jarmen423/agent-communication-server/main/scripts/install_remote.sh \
  | bash -s -- wss://hub.example.com:8080
export NATS_TOKEN='…'                     # from the hub operator
hub-delegate-remote --to claude-1 --from me --prompt "hi" --verbose
```

The installer downloads the release binaries for your platform over HTTPS and
verifies them against the release's `SHA256SUMS`. When no release asset exists
for your platform, it builds from source instead. Remote *workers* use
`packaging/remote/install.sh`, a Python-only bundle. See
[`docs/JOIN_HUB.md`](docs/JOIN_HUB.md) and [`docs/REMOTE_INSTALL.md`](docs/REMOTE_INSTALL.md).

### C. Orchestrate from Claude Code

```bash
claude plugin marketplace add jarmen423/agent-communication-server   # then install "nats-hub"
# or, from a checkout: claude --plugin-dir ./claude-code-plugin
```

The plugin's MCP server reads `NATS_URL` (default `nats://127.0.0.1:4222`),
`NATS_HUB_IDENTITY` and the usual `NATS_*` auth env vars.

## Release binaries

Each `v*` tag publishes `nats-hub-<version>-<target>.tar.gz` for
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (glibc ≥ 2.35) and
`aarch64-apple-darwin`. Every tarball holds all `hub-*` CLIs, including
`hub-server` and `hub-tui`. Alongside them are the Python remote-worker bundle
and `SHA256SUMS`. See [`docs/RELEASING.md`](docs/RELEASING.md).

## Use it as a crate

```toml
[dependencies]
nats-hub = { git = "https://github.com/jarmen423/agent-communication-server" }
# transport only, no SurrealDB/RocksDB:
# nats-hub = { git = "…", default-features = false, features = ["no-storage"] }
```

```rust
use nats_hub::HubClient;
use serde_json::json;

let client = HubClient::connect("nats://127.0.0.1:4222", "my-app").await?; // auth from NATS_* env
client.send_to("worker-1", "tasks", json!({"prompt": "process data"})).await?;
let mut inbox = client.subscribe_inbox().await?;
```

Feature flags, the `Storage` trait and embedding notes are in
[`docs/PORTABILITY.md`](docs/PORTABILITY.md).

## Documentation

| Start here | |
|---|---|
| [`CONTRIBUTING.md`](CONTRIBUTING.md) | Dev setup on any machine, how tests run, conventions |
| [`AGENTS.md`](AGENTS.md) | Architecture, code layout, subjects, CLI reference (for humans and AI agents) |
| [`refocus-iteration-2.md`](refocus-iteration-2.md) | Current iteration: definition of done per area, status board |

| Running a hub | |
|---|---|
| [`docs/OPERATOR_HUB.md`](docs/OPERATOR_HUB.md) | Stand up the central hub (systemd, firewall, TLS) |
| [`docs/SECURITY.md`](docs/SECURITY.md) | Auth model: tokens, TLS, per-agent credentials |
| [`docs/JOIN_HUB.md`](docs/JOIN_HUB.md) · [`docs/QUICK_START_REMOTE.md`](docs/QUICK_START_REMOTE.md) | Join a hub from a laptop |
| [`docs/REMOTE_INSTALL.md`](docs/REMOTE_INSTALL.md) · [`docs/REMOTE_AGENTS.md`](docs/REMOTE_AGENTS.md) | Remote workers: thin install and the WebSocket adapter |
| [`deploy/systemd/`](deploy/systemd/) | Unit files |

| Using it | |
|---|---|
| [`docs/WORKER_BACKENDS.md`](docs/WORKER_BACKENDS.md) | Worker types and backends (Claude Code, Codex, ACP, headless CLIs) |
| [`docs/BRIDGES.md`](docs/BRIDGES.md) | Human bridges (Telegram, Discord) |
| [`docs/VISUALIZER.md`](docs/VISUALIZER.md) | Browser visualizer: startup, tokens, troubleshooting |
| [`docs/PORTABILITY.md`](docs/PORTABILITY.md) | Embedding the crate, feature flags |
| [`docs/RELEASING.md`](docs/RELEASING.md) | Cutting a release, dry runs, install-from-release |

| Background | |
|---|---|
| [`docs/PRODUCT_VISION.md`](docs/PRODUCT_VISION.md) · [`docs/DATABASE_PLAN.md`](docs/DATABASE_PLAN.md) | Vision and persistence design |
| [`docs/archive/`](docs/archive/) | Finished phase plans and handoffs (historical) |

## License

<!-- license:start -->
BSL 1.1 — converts to Apache 2.0 on 2030-01-01. The SurrealDB Rust SDK is Apache 2.0. (A `LICENSE` file is still to be added; the plugin manifests currently say MIT. Tracked in `refocus-iteration-2.md` §7; see [`LICENSE.md`](LICENSE.md).)
<!-- license:end -->
