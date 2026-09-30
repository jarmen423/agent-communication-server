# Contributing / Dev Setup

This gets a fresh clone on any Linux or macOS machine building, testing, and
running a local hub. None of it needs sudo except the one-time system packages
in step 1. For the current sprint and task ownership, read
[`refocus.md`](refocus.md) first.

## 1. System prerequisites (one-time)

| Need | Why | Install |
|---|---|---|
| Rust (stable, ≥ 1.74) | the crate | <https://rustup.rs> |
| C/C++ toolchain + libclang | SurrealDB's embedded RocksDB is compiled from source through `bindgen` | Debian/Ubuntu: `sudo apt install build-essential clang libclang-dev` · Fedora: `sudo dnf install gcc-c++ clang-devel` · macOS: `xcode-select --install` |
| Python ≥ 3.10 | workers, bridges, MCP plugin server | system Python is fine; `uv` is used when present |
| Node ≥ 20 *(optional)* | only for the JS Cline workers (`worker.js`, `hub_worker.js`) | <https://nodejs.org> |

You do **not** need to install `nats-server` yourself. `make setup` downloads a
pinned, checksum-verified copy into `.tools/bin/`.

> **libclang without clang headers.** On Ubuntu, libclang is often installed
> without clang's builtin headers, and the RocksDB build then fails with
> `'stdbool.h' file not found`. Every `make` target and `scripts/dev/*` script
> detects this and sets `BINDGEN_EXTRA_CLANG_ARGS` to GCC's builtin include dir
> (`scripts/dev/bindgen_args.sh`). If you run bare `cargo` yourself, use:
>
> ```bash
> export BINDGEN_EXTRA_CLANG_ARGS="$(scripts/dev/bindgen_args.sh)"
> ```
>
> Keep this value stable between builds. Changing it forces a full RocksDB
> rebuild, which takes about 10 minutes.

## 2. Setup, check, build, test

```bash
git clone https://github.com/jarmen423/agent-communication-server.git
cd agent-communication-server

make setup        # nats-server → .tools/bin, Python venv → .venv (idempotent), then runs doctor
make doctor       # re-check the toolchain any time; prints the fix for anything missing
make build        # all binaries (incl. hub-tui). The first build compiles RocksDB (~10 min).
make test         # Rust + Python tests against a throwaway nats-server + hub-server
```

Optional extras:

```bash
make setup-extras # ACP / Cursor / Telegram / Discord SDKs
make setup-js     # node_modules for the JS Cline workers
make lint         # cargo fmt --check + clippy
```

### Where things go

| Path | What | Git |
|---|---|---|
| `target/` | cargo output (default) | ignored |
| `.tools/bin/` | pinned `nats-server` | ignored |
| `.tools/run/` | `make up` state: DB, JetStream, logs (`make clean-run` wipes it) | ignored |
| `.venv/` | Python venv | ignored |

**Disk:** a full debug build plus tests is about 10–30 GB. If your root disk is
tight, point cargo somewhere bigger. The scripts respect it:

```bash
export CARGO_TARGET_DIR=/big/disk/cargo-targets/nats-hub
```

## 3. How tests work

- `make test-rust` runs `scripts/dev/with_stack.sh cargo test --features tui`.
  `with_stack.sh` starts a **private** `nats-server` on a random port and a
  `hub-server` with a temp DB, exports `NATS_URL`, runs the command, and tears
  everything down. It never touches `:4222` or your real DB.
- `make test-py` runs `pytest tests/python` the same way. Tests marked `live`
  (for example the echo-worker round trip) run only under `with_stack.sh`.
- You can wrap anything:

  ```bash
  scripts/dev/with_stack.sh cargo test --test inbox_routing -- --nocapture
  KEEP_LOGS=1 scripts/dev/with_stack.sh .venv/bin/python -m pytest tests/python -k echo
  ```

- Tests that need a live NATS must read `NATS_URL` (default
  `nats://127.0.0.1:4222`). Never hard-code the port.
- CI (`.github/workflows/ci.yml`) runs fmt, build, and both test suites the same way.

## 4. Run a local hub

```bash
make up
# nats-server :4222 · hub-server + visualizer http://127.0.0.1:9191/ · echo workers echo-1, echo-2
```

In another terminal:

```bash
target/debug/hub-delegate --to echo-1 --prompt "hello" --verbose   # → echo: olleh
target/debug/hub-agents
target/debug/hub-tui            # if built with `make build`
```

Knobs: `NATS_PORT`, `WS_ADDR`, `WORKERS="echo-1 echo-2"`, `NO_BUILD=1`
(see `scripts/dev/up.sh`). If a `nats-server` is already listening on the port,
`make up` reuses it.

Real LLM workers (`hermes_acp_worker.py`, `cursor_worker.py`, …) run from the
venv, for example `.venv/bin/python hermes_acp_worker.py --identity hermes-1`.
They also need their CLI or SDK installed and authenticated; `make doctor`
lists which agent CLIs it can find. See `docs/WORKER_BACKENDS.md`.

## 5. Working conventions

- Read `AGENTS.md` (architecture, conventions) and `refocus.md` (current sprint,
  **write-scope ownership**, and the reply contract in §6).
- Branch per task (`refocus/<id>-<slug>`) and open a PR against `main`.
- Keep files under about 400 LOC. Async everywhere (tokio / asyncio). No blocking calls.
- Before pushing: `make lint && make test`.
- Never commit machine-specific paths (`/home/<you>`, custom target dirs).
  Use repo-relative paths or env vars with sensible defaults.

## 6. Troubleshooting

| Symptom | Fix |
|---|---|
| `'stdbool.h' file not found` (librocksdb-sys) | Use `make build`, or export `BINDGEN_EXTRA_CLANG_ARGS="$(scripts/dev/bindgen_args.sh)"`; or install the full `clang` package |
| `Unable to find libclang` | Install `libclang-dev` / `clang-devel`, or set `LIBCLANG_PATH` |
| RocksDB rebuilds every time | `BINDGEN_EXTRA_CLANG_ARGS` or the compiler env changed between builds. Keep them stable |
| `nats-server not found` | `make setup` (installs into `.tools/bin`) |
| `ModuleNotFoundError: nats` | Use `.venv/bin/python`, or `make setup` |
| `LOCK: Resource temporarily unavailable` | Another `hub-server` owns that DB. CLIs go through the query API, so use one hub-server per DB |
| Port 4222 in use | `NATS_PORT=4333 make up`, or reuse the running server |
