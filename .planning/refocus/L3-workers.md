# L3 — Claude Code + Codex workers, backend hardening, Python tests (local subagent)

**Branch:** `refocus/l3-workers` · **Status board:** `refocus.md` §5 row L3

## Read first
`refocus.md`: §3, §5 (**your write scope**) and §6 (the reply contract; the runtime implements it, and backends just return text); `CONTRIBUTING.md`; `docs/WORKER_BACKENDS.md`; `worker_runtime.py` (read only); `worker_backends/*`.

## Problem
The two agents we actually use aren't workers. `worker_supervisor.py` (~53) maps `claude` and `codex` to `echo_worker.py`, so picking "Claude" in the visualizer silently gets you an echo bot. Other backend problems:
- **HeadlessCli** (`worker_backends/headless_cli.py`):
  - `timeout_sec=None` by default; on timeout it calls `kill()` without `wait()` and doesn't kill the process group.
  - A non-zero exit that still produced stdout counts as success.
  - kilo/opencode pass the prompt positionally without `--`.
- **ACP stdio backends** (`grok_acp.py`, `opencode_acp.py`):
  - They pipe stderr and never read it (deadlock risk).
  - They advertise `fs`/`terminal` client capabilities they don't implement.
  - They hard-code the `allow-always` permission option.
- **Supervisor:**
  - Sends child output to DEVNULL, so failures are invisible.
  - Doesn't restart a crashed worker.
  - Waits 30s for "ready", while workers send their first heartbeat only *after* 30s.
  - Leaves orphaned children on exit.
- There are no Python tests for any of this.

## Deliverables
1. **`claude_worker.py`** as a HeadlessCli preset (or a small dedicated backend):
   - Runs `claude -p <prompt> --output-format stream-json --verbose`, with `--resume <session_id>` for session turns (capture `session_id` from the stream).
   - Parses NDJSON into progress callbacks and a final result.
   - Flags: `--repo` (cwd), `--model`, `--permission-mode` (default `acceptEdits`; `bypassPermissions` only with an explicit `--dangerously-skip-permissions`), `--allowed-tools`, `--timeout-secs`.
   - Check `claude --help` on this machine (v2.1.x) for exact flags.
2. **`codex_worker.py`:**
   - Runs `codex exec --json -C <repo> [--skip-git-repo-check] [-s <sandbox>] <prompt>`, with `codex exec resume <id>` for session turns.
   - Parses JSONL events into progress callbacks and a final message (`-o/--output-last-message` is a fallback).
   - Default sandbox `workspace-write`. `--dangerously-bypass-approvals-and-sandbox` is only allowed via an explicit flag.
   - Check `codex exec --help` (v0.159).
3. **Supervisor:**
   - Map `claude`/`codex` to the new workers and remove the echo fallback.
   - Log children to `.tools/run/workers/<identity>.log`.
   - Restart with backoff on crash (bounded).
   - Clean up children on exit (SIGTERM the process groups).
   - Treat "ready" as the first heartbeat *or* the inbox subscription being established.
   - Update `config/provider_models.json` / `model_catalog.py` so claude and codex show real models.
4. **HeadlessCli hardening:**
   - Default timeout of 900s.
   - On timeout, kill the process group and `await wait()`.
   - Non-zero exit is an error (include a stderr tail).
   - Insert `--` before positional prompts where the CLI supports it.
   - Drain stderr concurrently.
5. **ACP stdio hardening** for grok and opencode:
   - Drain stderr into a bounded buffer.
   - Stop advertising `fs`/`terminal`, or implement them minimally.
   - Pick the permission option from the offered `options` by kind (prefer `allow_once`/`allow_always` per config).
   - Mark the backend dead and restart the agent process on EOF.
   - *Stretch:* extract the shared JSON-RPC plumbing into `worker_backends/acp_stdio.py`.
6. **Tests** in `tests/python/test_worker_*.py`:
   - Use **fake CLIs**: tiny scripts in `tests/python/fixtures/` that emit canned stream-json/JSONL, sleep past a timeout, or exit non-zero.
   - Cover command building, NDJSON parsing, resume-id capture, timeout kill (assert no orphan), and non-zero exit.
   - Add one `live` test (under `with_stack.sh`) running `claude_worker.py` with a fake `claude` on PATH end to end through the hub.
7. **Docs:** update `docs/WORKER_BACKENDS.md` (claude and codex sections, flags, safety defaults).

## Real-CLI smoke (allowed, keep it cheap)
`claude` and `codex` are installed and authenticated on this machine. After unit tests pass, run **one** trivial real round-trip each (prompt: `Reply with exactly: pong`) through `make up`-style infra, using `scripts/dev/with_stack.sh` or your own stack on a free port; don't reuse :4222. Use `--repo` pointing at a temp dir. Paste the transcript in your report. Do **not** run open-ended prompts or bypass-permissions modes.

## Constraints
- **Write scope:** `worker_backends/**`, `*_worker.py` (not `echo_worker.py`), new `claude_worker.py` / `codex_worker.py`, `worker_supervisor.py`, `config/provider_models.json`, `docs/WORKER_BACKENDS.md`, `tests/python/test_worker_*.py`, `tests/python/fixtures/**`.
- Don't touch `worker_runtime.py`, `worker_events.py` or `nats_connect.py`; another agent owns the runtime and reply contract. If you need a runtime hook, describe it in your report.
- No Rust changes. Don't edit `README.md`, `AGENTS.md`, `refocus.md`, `Makefile` or `requirements*.txt`; propose any changes.
- Files ≤ ~400 LOC. asyncio subprocesses only.

## Definition of done
`make test-py` green (paste the real tail) plus the two real smoke transcripts. `make test-rust` must be unaffected.
