# nats-hub

**A message hub for your coding agents: Claude Code, Codex, Cursor, Hermes and
others, on one machine or many.** An orchestrator (you in a terminal, or an
agent through the MCP plugin) delegates a task to a worker anywhere on the hub
and gets a structured result back. You can watch the whole conversation live
and query it later.

nats-hub is one `nats-server`, one Rust router (`hub-server`), and thin
clients: CLIs, Python workers, an MCP server, a terminal dashboard and a
browser visualizer. Workers on other machines dial in over `wss://` with a
token or credentials. They don't run a server of their own. You can use the
hosted service, ACS (see the [hosted quick start](#hosted-quick-start)), or
run your own hub.

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

## Hosted quick start

Hosted ACS at `https://acs.agentmemorylabs.com` runs the hub for you: no
server, no checkout, no hub URL to type. You need Linux (x86_64 or arm64,
glibc ≥ 2.35) or macOS on Apple silicon. On Windows, use the web connector
below.

1. **Install** `acs` and the `hub-*` CLIs. The installer verifies them against
   the release's `SHA256SUMS` and links them into `~/.local/bin`; if that
   isn't on your `PATH`, it prints the line to add.

   ```bash
   curl -fsSL https://raw.githubusercontent.com/jarmen423/agent-communication-server/main/scripts/install_remote.sh | sh
   ```

2. **Sign in.** Approve this device in your browser. `acs login --headless`
   prints a link to open on another device instead.

   ```bash
   acs login
   ```

3. **Add an identity** for each agent on this machine. An identity is an
   orchestrator unless you add `--role worker`.

   ```bash
   acs agent add me
   acs agent add claude-1 --role worker
   ```

4. **Start the credential daemon.** It runs as a user service (systemd on
   Linux, launchd on macOS) and keeps every identity's credentials fresh.

   ```bash
   acs up
   ```

5. **Connect an agent.** Either let Claude Code orchestrate through MCP:

   ```bash
   acs mcp install claude --identity me
   ```

   or run Claude Code as a worker. It needs the `claude` CLI installed and
   logged in, and it stays in the foreground. It can run on this machine or
   on any other machine signed in to the same account.

   ```bash
   acs worker run claude --identity claude-1 -- --repo ~/code/my-project
   ```

6. **Delegate** to a running worker, here the one from step 5 (from another
   terminal or machine). `acs exec` runs any `hub-*` CLI as one of your
   identities: it sets `NATS_URL`, `NATS_CREDENTIALS_FILE` and
   `NATS_HUB_IDENTITY`.

   ```bash
   acs exec me -- hub-delegate --to claude-1 --prompt "Summarize README.md" --verbose
   ```

   To drop the prefix for one shell, run `eval "$(acs env me)"`. After that,
   `hub-delegate …`, `hub-agents` and `hub-history --tail` work as they are.

**From claude.ai or ChatGPT** (nothing to install, so it also works on
Windows), add a custom connector with the URL
`https://acs.agentmemorylabs.com/mcp`. In claude.ai that's Customize →
Connectors → "+" → Add custom connector. Sign in and approve. The chat gets
its own identity on your account, and it can list your agents, delegate to
your workers and read their results. See [`docs/REMOTE_MCP.md`](docs/REMOTE_MCP.md).

## Self-hosted hub: join it from another machine (no Rust needed)

```bash
curl -fsSL https://raw.githubusercontent.com/jarmen423/agent-communication-server/main/scripts/install_remote.sh \
  | sh -s -- wss://hub.example.com:8080
export NATS_TOKEN='…'                     # from the hub operator
hub-delegate-remote --to claude-1 --from me --prompt "hi" --verbose
```

With a hub URL, the installer also writes `hub-delegate-remote`, a
`hub-delegate` wrapper with that URL baked in. Every `hub-*` CLI also reads
`NATS_URL` (for `--nats-url`) and `NATS_HUB_IDENTITY` (for `--from`), so
`export NATS_URL=wss://hub.example.com:8080 NATS_HUB_IDENTITY=me` works
too. When no release asset exists for your platform, the installer builds
from source. Remote *workers* use `packaging/remote/install.sh`, a
Python-only bundle. See [`docs/JOIN_HUB.md`](docs/JOIN_HUB.md) and
[`docs/REMOTE_INSTALL.md`](docs/REMOTE_INSTALL.md).

## Local development (from source)

These run everything on your machine, for working on nats-hub itself.

### A. Run a hub locally

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

The CLIs default to `$NATS_URL`, then `nats://127.0.0.1:4222`. Run them in
a shell that has no hosted `NATS_URL` exported (from `acs env`).

Swap the echo worker for a real agent. The agent's CLI must be installed and
logged in:

```bash
.venv/bin/python claude_worker.py --identity claude-1     # Claude Code
.venv/bin/python codex_worker.py  --identity codex-1      # Codex
./target/debug/hub-delegate --to claude-1 --prompt "Summarize README.md" --verbose
```

### B. Orchestrate from Claude Code

```bash
claude plugin marketplace add jarmen423/agent-communication-server   # then install "nats-hub"
# or, from a checkout: claude --plugin-dir ./claude-code-plugin
```

The plugin's MCP server reads `NATS_URL` (default `nats://127.0.0.1:4222`),
`NATS_HUB_IDENTITY` and the usual `NATS_*` auth env vars. For hosted ACS,
use `acs mcp install` (step 5 above) instead.

### C. Preview the hosted flow (all local, DEV ONLY pieces)

This runs the hosted flow against a stub control plane on a local
operator-mode stack (`docs/HOSTED_PLAN.md`):

```bash
scripts/dev/install_nsc.sh
scripts/dogfood_hosted.sh                      # acs login → agent add → mcp install → delegate,
                                               # wave, file lock, backup/restore; remote MCP; visualizer
scripts/dogfood_hosted.sh --real-worker claude # also delegate a one-word prompt to Claude Code
make demo-remote-mcp                           # the endpoint web chat apps (claude.ai, ChatGPT) connect to
```

Guides: `docs/ACS_CONNECTOR.md` (connector), `docs/REMOTE_MCP.md` (web chat
apps), `docs/OPERATOR_MODE.md` (tenancy), `docs/FILE_LOCKS.md`.

## Release binaries

Each `v*` tag publishes `nats-hub-<version>-<target>.tar.gz` for
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` (glibc ≥ 2.35) and
`aarch64-apple-darwin`. Every tarball holds `acs` and all `hub-*` CLIs,
including `hub-server` and `hub-tui`. Alongside them are the Python remote-worker bundle
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
nats-hub is licensed by path. [`LICENSING.md`](LICENSING.md) has the full map.

- **Client side: Apache-2.0.** This covers the MCP server and plugins, the Python workers and bridges, the remote installers, the Rust client library and every client CLI (`hub-delegate`, `hub-watch`, `hub-tui`, …).
- **Server side: BUSL-1.1.** This covers `hub-server`, `hub-admin` and what only they run: router, query API, orchestrator, storage, analytics and metrics, the WS bridge and visualizer, and the deploy configs. You may use it in production, including across your organization. You may not offer it, or a service substantially derived from it, to third parties as a hosted or managed service. Each version converts to Apache-2.0 four years after its first public release.

Texts are in [`LICENSES/`](LICENSES/), and the machine-readable map is [`REUSE.toml`](REUSE.toml). The default build embeds SurrealDB, which is under SurrealDB's own BSL. Its grant forbids offering SurrealDB as a database service. nats-hub never exposes the database, and the `no-storage` build doesn't include it.
<!-- license:end -->
