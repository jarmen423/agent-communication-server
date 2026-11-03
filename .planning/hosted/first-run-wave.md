# First-run wave: hosted install to first delegation with no help

**Goal:** a new user on Linux or macOS follows the README and gets from
nothing to a working delegation on hosted ACS. That means no checkout, no
second installer, no `--server` flag and no hand-set env vars. This wave
then ships as **v0.0.3**.

The findings come from the owner dogfood of 2026-11-03. Read
`.planning/hosted/dogfood-2026-11-03.md` first: B1–B4 and F1–F4 are the
scope here. B5 (Windows) and O1 (acs-private) are out of scope.

## Shared interface (both tasks write to this; don't change it alone)

- **Hosted server.** `https://acs.agentmemorylabs.com`. The NATS URL comes
  from `acs login` and is stored in the device state; nobody types it.
- **`acs login`.**
  - `--server`/`ACS_SERVER` defaults to the hosted server.
  - Dev and self-host pass `--server http://127.0.0.1:9300` or set
    `ACS_SERVER`. Update every test, script and doc that relied on the
    old default.
- **`acs env <identity>`.** Prints POSIX `export` lines for these
  variables, so `eval "$(acs env laptop)"` works:
  - `NATS_URL`
  - `NATS_CREDENTIALS_FILE`
  - `NATS_HUB_IDENTITY`
- **`acs exec <identity> -- <cmd> [args…]`.** Runs `cmd` with those three
  variables set, and strips the other `NATS_*` auth variables, the same
  way `acs worker run` does today.
- **`hub-*` CLIs.**
  - `--nats-url` defaults to `$NATS_URL`, then `nats://127.0.0.1:4222`.
  - The sender identity (`--from` on `hub-delegate`, `hub-publish`,
    `hub-session`, `hub-wave` and the like) defaults to
    `$NATS_HUB_IDENTITY`.
  - `ApiClient` users keep reading `NATS_HUB_IDENTITY`.
- **`acs worker run claude --identity <id> -- <worker args>`.** Works from
  a release install alone. Add `codex` and `echo` too if cheap.

The hosted quick start in the README (Task B) is exactly:

1. `curl … install_remote.sh | sh`
2. `acs login`
3. `acs agent add <id> [--role worker]`
4. `acs up`
5. Either `acs mcp install claude --identity <id>` or
   `acs worker run claude --identity <id> -- --repo <path>`.
6. `acs exec <id> -- hub-delegate --to <worker> --prompt "…"`

Include a claude.ai/ChatGPT connector paragraph with the URL
`https://acs.agentmemorylabs.com/mcp`.

## Task A: `acs` connector (branch `first-run/acs`, worktree `/data/code/acs-wt/first-run-acs`)

**Write scope (only these paths):**
- `crates/acs/**`
- `packaging/remote/**`
- `docs/ACS_CONNECTOR.md`
- `scripts/dogfood_hosted.sh` and other scripts that call `acs login`
  without `--server`
- tests for the above, including the `make test-acs` e2e

**Work items:**
- **B1. Ship the worker runtime inside `acs`.**
  - Do it the same way `acs mcp install` already ships the MCP modules
    (see `crates/acs/build.rs` and `src/runtime.rs`). Embed what
    `claude_worker.py` needs: `worker_runtime.py`, `worker_events.py`,
    the `worker_backends` modules it imports, `nats_connect.py`, and
    anything else it imports transitively. Do the same for `codex` and
    `echo` if cheap.
  - On first `acs worker run`, materialize them into a private,
    versioned directory with a private venv (reuse the MCP venv logic).
  - Keep `ACS_WORKER_DIR`/`ACS_PYTHON` as overrides. Keep the rule that
    code is never run from the current directory.
  - Prove it with a test that runs `acs worker run echo` (or `claude`
    against the fake CLI in `tests/python/fixtures/`) with **no**
    `ACS_WORKER_DIR` and no checkout on the path.
- **B3.** Change the `acs login` default server as specified in the
  shared interface.
- **F2 (acs side).** Add `acs env` and `acs exec` as specified.
- **F3.** `acs status` shows credential expiry as RFC 3339 plus a
  relative time ("in 23h"), not as an epoch.
- **F1.**
  - `packaging/remote/install.sh` defaults to
    `~/.local/share/nats-hub/remote`, the path `acs` looks in.
  - Its closing message names `acs worker run`.
  - Note in `docs/ACS_CONNECTOR.md` that hosted users don't need it.
- **Docs.** Update `docs/ACS_CONNECTOR.md` for all of the above.

## Task B: CLIs, installer, MCP, README (branch `first-run/cli`, worktree `/data/code/acs-wt/first-run-cli`)

**Write scope (only these paths):**
- `crates/client/src/bin/**`
- `crates/client/src/connect_opts.rs`
- `crates/client/src/query_api_client.rs`
- `scripts/install_remote.sh`
- `packaging/release/**`
- `mcp_server/**`, then `scripts/dev/sync_plugins.sh` (which regenerates
  the plugin copies)
- `README.md`
- `AGENTS.md` (CLI table only)
- `tests/python/test_install_release.py` and other tests for these paths

**Work items:**
- **B2.** `install_remote.sh`:
  - symlinks every installed binary into `~/.local/bin`, creating it if
    missing;
  - if that directory isn't on `PATH`, prints the exact line to add;
  - is idempotent on re-run;
  - is covered by `test_install_release.py`.
- **B4.**
  - The installer's closing message leads with the hosted next step
    (`acs login`) and mentions self-host in one line.
  - The README gets a **hosted quick start** near the top, exactly as in
    the shared interface. The local-dev section stays and is labeled as
    such.
- **F2 (CLI side).**
  - Add the `NATS_URL` and `NATS_HUB_IDENTITY` defaults from the shared
    interface, using clap `env = …` where possible.
  - When a query or request times out and no identity was set, say so:
    "no identity set (NATS_HUB_IDENTITY / --from); a bound-identity hub
    drops unbound requests". Don't just print "deadline has elapsed".
- **F4.** MCP `delegate_task`/`delegate_async`: an argument validation
  error lists the valid argument names. Fix it in `mcp_server/`, then
  run `scripts/dev/sync_plugins.sh`.

## Rules for both

- Work only in your worktree and your write scope. If you need a change
  outside it, stop and say so in your report.
- **Commits.**
  - Commit on your branch, in small logical commits.
  - Commit messages end with
    `Co-Authored-By: <your model> <noreply@anthropic.com>`, or your
    provider's equivalent.
  - **Do not push and do not merge.** The orchestrator verifies and
    merges.
- **Conventions.**
  - Follow `AGENTS.md`: files under ~400 LOC, async everywhere, licensing
    by path, and no per-file license headers.
  - Before running cargo directly, run `source scripts/dev/lib.sh`.
- **Verify before reporting.** Run these and include the results in your
  report:
  - `make lint`
  - `make test` (or at least `make test-rust` plus `make test-py`)
  - `make test-acs`
  - `make check-no-storage`
- **Disk.** `/data` is shared and has filled up before. When you finish,
  run `cargo clean` in your worktree; the orchestrator rebuilds when
  verifying.
- **Report.**
  - What changed, file by file.
  - The exact test commands and results.
  - Anything you deliberately didn't do, and why.
  - Open questions.
