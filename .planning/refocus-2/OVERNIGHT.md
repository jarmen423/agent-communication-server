# Overnight run 2026-10-21: orchestrator handoff

Josh is offline overnight. He authorized running the rest of the wave plan
without checking in, and will check in in the morning for live e2e testing.
This file holds the plan and its state, so any orchestrator session (or this
one after context compaction) can continue. **Update the log at the bottom
after every state change.**

## Ground rules

- **Who merges.**
  - The orchestrator (the main checkout, `/data/code/agent-communication-server`, on `main`) verifies and merges.
  - Task agents work in `/data/code/acs-wt/<task>` on `iter2/<task>`. They push their branch for CI. They never push or merge to main.
- **Verification before every merge.**
  1. Read the diff, looking hardest at security-sensitive surfaces: auth, paths, permissions, anything that writes files or mints credentials.
  2. Fix small things yourself on the branch.
  3. Merge into main locally.
  4. Run the full gates on the merged main: `make lint`; `make test`; `make test-bound`; `make test-durable`; `cargo check` no-storage (or its post-split equivalent); `scripts/dev/operator_stack.sh .venv/bin/python -m pytest -q tests/python/test_operator_isolation.py tests/python/test_operator_credentials.py`; and the dogfoods relevant to the change.
  5. Push main.
  6. Update the board in `refocus-iteration-2.md` and the log here.
- **Raw cargo.** `source scripts/dev/lib.sh` first, for the kache wrapper. Without it, kache's read-only hardlinks break builds. Also `export BINDGEN_EXTRA_CLANG_ARGS="$(scripts/dev/bindgen_args.sh)"`.
- **Delegation.**
  - Use T3 `delegate_task` with the provider and model below, a self-contained prompt, and `runtimeMode: full-access`.
  - Native Claude subagents (the Agent tool) are fine for research or review.
  - If an agent fails, retry once with a new `clientRequestId`, or on another provider. If it fails again, record it and move on.
- **Out of scope tonight.**
  - Nothing that costs money or creates external resources: cloud infra (H-3), new repos, a private control plane (H-2), billing.
  - Publishing releases or tags.
  - Merging anything that weakens auth.

## Providers (from `orchestrator_capabilities`)

- `codex` / `codex_2` / `codex_3`: gpt-6.1-sol (option `reasoningEffort`: high)
- `claudeAgent_2`..`_4`: claude-opus-5-5
- `acpRegistry_devin`: swe-2-high
- `muse`, `muse_muse_code_2`: muse-wonder-1.0
- `grok`: grok-4.7

## Plan

### Wave A (started 2026-10-21 ~02:30 UTC)

| Task | Brief | Provider | Depends on |
|---|---|---|---|
| H-1a crate split (Apache client crate + BUSL server crate) | `H-1a-crate-split.md` | codex gpt-6.1-sol high | — |
| H-1b remote MCP endpoint + dev stub control plane (OAuth) | `H-1b-remote-mcp.md` | claudeAgent_2 opus-5-5 | — |

### Wave B (start each task as soon as its dependencies are merged)

| Task | Provider | Depends on | Brief to write (at launch, against the merged layout) |
|---|---|---|---|
| **H-1c `acs` connector** | codex gpt-6.1-sol high | H-1a, H-1b | Covers `crates/acs` (or its post-split home), Rust, Apache, built on the client crate, and these commands: `acs login` (device-code flow against the stub control plane: add device authorization + token endpoints and per-device identity minting to `stub_control_plane/`), `acs login --headless`, `acs login --key`, `acs up/down` (daemon: refresh creds before expiry, atomic rename into `~/.config/acs/creds/<id>.creds`, systemd-user/launchd unit generation), `acs status`, `acs agent add/rm`, `acs mcp install claude\|codex\|hermes`. **Fold in two H-0 follow-ups:** (a) derive the reply-inbox identity from the creds JWT `name` claim, so the `NATS_USER=hub-server` hint is unnecessary (`connect_opts.rs` + `nats_connect.py`); (b) replace the nats-py private-method hook in `nats_connect.py` with a public mechanism if nats-py offers one; otherwise keep it, pin nats-py, and comment why. **E2E:** stub CP + operator stack + durable hub; `acs login` (headless, scripted approve) → `acs agent add` → `acs mcp install claude` into a temp HOME → a delegate through the installed MCP config to an echo worker; credential refresh survives an expiry. |
| **W2-D file-lock broadcast** | acpRegistry_devin swe-2-high | H-1a | Criteria: `refocus-iteration-2.md` §2 "New feature" + `TODO.md`. An agent announces "working on file X" (lock with TTL and holder identity, released explicitly or on expiry or holder death via presence). Others get notified and can wait for release. Server-side state in hub-server (persisted, survives restart), API ops with owner-scoped writes (only the holder releases; admins can force), `hub-lock` CLI, MCP tools (`lock_files`, `release_files`, `wait_for_files`, `list_locks`), worker/wave integration optional. Works in bound and durable modes. Write scope: new server module + query-API handlers, client methods, MCP tools (+ sync_plugins), CLI, tests, `docs/FILE_LOCKS.md`. Must not touch delivery, operator scripts, visualizer. |
| **W2-C visualizer/observability** | muse muse-wonder-1.0 (if it struggles: codex_2) | H-1a (TUI moved) | Criteria: `refocus-iteration-2.md` §2 "Observability": split the visualizer into ES modules with p5 vendored (works offline); **replace the third-party Petdex sprites** with a theme manifest and an original default theme we can redistribute (remove `visualizer/pets/**` third-party art and the `LicenseRef-Petdex-Unverified` entry in REUSE/LICENSES; keep `reuse lint` green); a Playwright smoke test (load, connect with token, an agent appears) in CI; the TUI works against a bound-identity hub (`--require-bound-identity`, identity from env). Write scope: `visualizer/**`, `docs/VISUALIZER.md`, `docs/pets.md`, `src/ws_bridge/**` only if needed for the smoke test, TUI module, new `tests/e2e_visualizer/**` + CI step, REUSE/LICENSES entries for the sprites. |

### Wave C (after wave B)

1. **Integration review** of everything merged tonight: one Claude review subagent over the full diff since `0d74bf5`, focusing on auth, credentials and files. Fix whatever is confirmed.
2. **Live e2e by the orchestrator** on a real operator-mode, durable, bound stack, with real output recorded in the log:
   - stub control plane, `acs login` / `agent add` / `mcp install`;
   - echo worker plus one real CLI worker if installed (e.g. `codex`/`claude`; keep the prompts tiny);
   - delegate through the stdio MCP and through the remote MCP endpoint (SDK client), a wave of two tasks, a file lock between two agents;
   - backup → restore;
   - visualizer smoke.
3. **Docs pass:** README quick start for the hosted flow; AGENTS.md test counts; `docs/HOSTED_PLAN.md` §10 status.
4. **Morning summary** for Josh at the top of this file, plus the final message in the thread.

## Log

- 2026-10-21 ~02:30 UTC: plan written. W2-A, W2-B, H-0, H-0b and the license split were already merged (main `0d74bf5`). Launching wave A.
- 2026-10-21 02:29 UTC: wave A launched.
  - H-1a: taskId `node:delegated-task:command%3Amcp%3Aa1b83716-7188-494a-b103-917c16a5ab8e%3Adelegate-task%3Ah1a-crate-split-r1` (codex gpt-6.1-sol high), worktree `/data/code/acs-wt/h1a-crate-split`.
  - H-1b: taskId `node:delegated-task:command%3Amcp%3Aa1b83716-7188-494a-b103-917c16a5ab8e%3Adelegate-task%3Ah1b-remote-mcp-r1` (claudeAgent_2 opus-5-5 high), worktree `/data/code/acs-wt/h1b-remote-mcp`.
  - Heartbeat: scheduled task `overnight-heartbeat-2026-10-21`, hourly, bound to this thread. Delete it after wave C.
- 02:30 UTC: H-1a r1 on `codex` failed immediately: usage limit ("try again at 4:32 AM"), no changes made. Relaunched as r2 on `codex_2`: taskId `node:delegated-task:command%3Amcp%3Aa1b83716-7188-494a-b103-917c16a5ab8e%3Adelegate-task%3Ah1a-crate-split-r2-codex2`. For later waves, `codex` (account 1) is out until about 04:32 local. Prefer `codex_2`/`codex_3` or Claude accounts 3 and 4.
