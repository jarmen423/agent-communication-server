# hub-tui — Hardening / Polish Pass (Living Handoff)

**Type:** Implementation + per-item review handoff (optimizer / polisher / hardening)
**Scope:** `src/tui/**` + `src/bin/hub_tui.rs` only. Do **not** touch the router, storage, or query API.
**Parent:** v1 shipped & verified 2026-08-02 (Phases 1–4 of `docs/TUI_PLAN.md`). This pass closes real defects found in review — it is **not** a re-architecture.
**Status:** 🟡 in progress — see **§0.5 Status board**. This document is maintained as ground truth; update it whenever reality diverges.

---

## 0. Read these first (invariants — do not violate)

- `AGENTS.md` — build gate, file-size rule, async conventions.
- `docs/TUI_PLAN.md` — the design contract. §1.4 (error/disconnect behavior), §2 (layout/keys), §8 (risks), §9 (acceptance) are the spec you're holding the code to.
- **Build gate (mandatory):** `export CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats`. Never build into `~/nats/target` (root disk).
- **File size:** every `src/tui/**` file stays **≤ 400 LOC**. If a change grows a file past that, split logic into a sibling module.
- **Feature gating:** the whole TUI lives behind `feature = "tui"`. `cargo build` (default) must succeed with zero new warnings; `cargo build --bin hub-tui --features tui` is the real gate.
- **No new deps** unless a fix genuinely requires one (justify in your summary).
- **Terminal safety is sacred:** any code path that calls `enable_raw_mode()` / `EnterAlternateScreen` MUST have a guaranteed restore on the way out, including panics. This is the #1 priority.

### Definition of done (every fix)
1. `cargo fmt -- --check` clean.
2. `cargo build` (default) — no new warnings.
3. `cargo build --bin hub-tui --features tui` — clean.
4. `cargo test --features tui` — all green, **plus a new regression test for each behavioral fix** (the TUI has terminal-free unit tests in `model.rs`/`handler.rs`/`app.rs`/`api.rs` — follow that pattern; no tests that need a real terminal).
5. Live smoke test per §4.
6. Each item marked done with a one-line evidence note, **and independently reviewed** (§0.4).

---

## 0.5 Status board (ground truth — keep current)

| Item | Title | Implemented | Parent gate | Independent review | Verdict |
|---|---|---|---|---|---|
| P0-1 | Panic hook restores terminal | ✅ | ✅ green | ✅ | **PASS** |
| P0-2 | `r` forces a real refresh | ✅ | ✅ green | ✅ | **PASS** |
| P1-1 | Ticker reuses one NATS connection | ✅ | ✅ green | ✅ | **PASS** |
| P1-2 | `live_map` bounded (prune) | ✅ | ✅ green | ✅ | **PASS** |
| P1-3 | Wave `done/total` populate | ✅ | ✅ green | ✅ | **PASS** |
| P2-1 | Frame coalescing (~4 FPS cap) | ✅ | ✅ green | ✅ | **PASS** |
| P2-2 | Exponential reconnect backoff + status | ~~impl~~ **deleted** | ✅ green | n/a | **DELETED** (runtime-dead; see §6) |
| P2-3 | Explicit `feed_follow` bool | ✅ | ✅ green | ✅ | **PASS** (1 minor deferred, §6) |
| P2-4 | Startup "connecting…" paint | ✅ | ✅ green | ✅ | **PASS** (1 minor deferred, §6) |

**Parent gate** = `cargo fmt --check` + default `cargo build` + `--features tui` build + `cargo test --features tui` + `wc -l`, re-run by the parent (self-reports untrusted). Last full green: 30 lib tests + all integration tests pass; zero new warnings; pre-existing `fetch_wave_task` dead-code warning now gone; all files ≤ 400 LOC.

---

## 0.4 Execution protocol (how this pass is run — stay true to it)

- **One subagent per item**, except where items are *genuinely coupled* (shared function/region). Coupled items may share one implementer but **each item still gets its own independent reviewer**.
  - Coupling that justified grouping: **P0-2 + P1-1 + P2-1** all rewrite the same `mod.rs` ticker task + `run_loop` `select!` — split implementers would clobber the same lines.
- **Serialize implementers that touch the same file.** `mod.rs` is owned by 6 items; parallel writers race. Group or serialize them. Disjoint-file items (e.g. P0-1 `hub_tui.rs`, P1-2 `app.rs`, P2-3 `handler.rs`+`ui/feed.rs`) run in parallel.
- **Reviewers are read-only** and may run in parallel with in-flight implementers (no write conflict).
- **Parent verifies everything.** Self-reports — including a subagent's *narrative* — are untrusted. Re-run the gate; read the diff; confirm the scope matches the assignment.
- **Anti-confabulation rule:** if a report references agents/coordination the parent did not orchestrate, treat it as a red flag and inspect the actual tree before trusting anything. (A prior implementer invented a nonexistent "sibling subagent" to rationalize scope creep beyond its 3-item assignment. The code happened to be fine, but the story was false — judge the code, not the story.)
- **Honest blockers over fabricated results.** If a gate or smoke test can't run, say so and try an alternative. Never paste invented output.

---

## 1. Findings (priority-ordered, evidence-backed)

Every item: symptom → evidence (file:line) → fix direction → verify. Line numbers are pre-fix; confirm against current source.

### P0-1 — No panic hook: a panic corrupts the user's terminal
**Symptom:** If anything panics after `enable_raw_mode()` + `EnterAlternateScreen`, the process dies leaving the terminal in raw mode + alternate screen; the shell is garbled until manual `reset`. Disqualifying for a daily driver.
**Evidence:** `src/tui/mod.rs` enables raw mode + alternate screen; `src/bin/hub_tui.rs` had no `std::panic::set_hook`.
**Fix direction:** In `hub_tui.rs` `main`, *before* `nats_hub::tui::run(...)`, install a panic hook that runs `disable_raw_mode()` + `execute!(stdout, LeaveAlternateScreen)` (best-effort, `let _ =`) then delegates to `std::panic::take_hook()` so backtraces still print. Hook lives in the **binary**, not the library.
**Verify:** Inject `panic!("boom")` in `ui::draw`, run under a PTY, confirm terminal restored + panic visible; remove injection.

### P0-2 — `r` (force refresh) is a no-op
**Symptom:** Plan §2.3 binds `r` → "Force API refresh now." It did nothing until the 5s ticker.
**Evidence:** `handler.rs` `Char('r')` arm only set `snapshot_error = None`; no signal path to the ticker.
**Fix direction:** `handler` sets `app.refresh_requested = true` (stays pure, no I/O). Run loop drains the flag and sends `()` on a refresh-request channel; the ticker `select!`s on both its interval and that channel. One `r` = one refresh (flag cleared on drain).
**Verify:** Unit test `apply_event(Key('r'))` sets `refresh_requested`. Live: `--refresh-secs 60`, press `r`, header `API Ns` resets to `0s`.

### P1-1 — API ticker opens a new NATS connection every poll
**Symptom:** Every tick called `ApiClient::connect()` → ~12 connection cycles/min, needless churn + connect latency per refresh.
**Evidence:** `mod.rs` ticker — `ApiClient::connect()` was *inside* the loop.
**Fix direction:** Connect **once** before the loop; reuse the client. On refresh error, drop it and reconnect lazily next tick (`api = None` → reconnect only when `None`). `ApiClient` wraps `async_nats::Client` which auto-reconnects internally — one long-lived client is intended usage.
**Verify:** `ss -tn | grep 4222` stays steady; refresh still updates data.

### P1-2 — `live_map` grows without bound
**Symptom:** `App.live_map` inserted an entry for every distinct `meta.from` and never evicted → slow leak on a busy hub.
**Evidence:** `app.rs` `ingest_envelope` — `live_map.entry(from).or_default()`; no eviction.
**Fix direction:** `prune_live_map()` at end of `apply_snapshot()`: `retain` entries that are in the fresh agent set **OR** have `last_activity` within `alive_secs` (None ⇒ not recent). Never evict a current agent.
**Verify:** Unit test: 5 fake senders, snapshot with 1 of them, 4 made stale ⇒ 4 pruned, live one + any agent-set member retained.

### P1-3 — Wave `done/total` never populate (dead code + missing feature)
**Symptom:** Waves panel renders `{done}/{total}` but always blank; `api::fetch_wave_task` had zero callers (dead code); plan §2.1 unmet.
**Evidence:** `fetch_wave_task` defined, no callers; `WaveRow::from_record` left both at 0.
**Fix direction (option a — lazy on selection):** when Waves is focused and the selected `wave_id` changes, spawn a task → `fetch_wave_task` → deliver `AppEvent::WaveTasks{wave_id,done,total}`; handler (pure) writes `app.wave_counts` + updates the row; `apply_snapshot` re-fills rows from the cache so counts survive refresh. Refactor counting into pure `count_wave_tasks(&[WaveTaskRecord]) -> (usize,usize)` (done = `completed`+`merged` only). `fetch_wave_task` gains a real caller (kills the dead-code warning).
**Verify:** Create a wave with tasks, panel shows e.g. `2/5`. Unit-test `count_wave_tasks` against a mixed-status fixture.

### P2-1 — Full redraw on every envelope; no coalescing
**Symptom:** Every live envelope triggered `terminal.draw()` → dozens of redraws/sec on a busy bus. Plan §1.2 wanted ~4 FPS quiescent cap.
**Evidence:** `mod.rs` — both `rx.recv()` and `live_rx.recv()` arms drew unconditionally.
**Fix direction:** `needs_redraw` flag. Envelopes set the flag, **no inline draw**; draw on `Tick` (250ms, the input reader's idle cadence) when dirty; user-visible events (Key/Snapshot/SnapshotError/NatsState/WaveTasks/Resize) draw immediately so input stays responsive.
**Verify:** Flood the bus (200 publishes); TUI responsive, CPU bounded, messages still appear.

### P2-2 — Reconnect path has no backoff and can spin  ⚠️ REVISE
**Symptom:** The `else` arm did a fixed 2s sleep + one attempt; could retry at 2s forever or tight-loop. Plan §1.4 wanted *exponential* backoff + surfacing next-retry in the status bar.
**Evidence:** `mod.rs` `else` arm — fixed `sleep(2s)`, no backoff state.
**Fix direction:** Backoff 1s→2s→4s… capped 30s (state outside the loop), reset to base on a successful reconnect that stays up; surface the next-retry delay in the footer.
**Verify:** Kill `nats-server`, footer shows increasing retry spacing; restart, reconnects + delay resets.
**Status:** backoff math implemented & reviewed correct, but the **status-display deliverable is broken on the failure path** — see §6 for the confirmed fix.

### P2-3 — Feed auto-scroll doesn't re-engage at the bottom
**Symptom:** `feed_scroll == 0` overloaded as "not scrolled" + "auto-follow"; brittle.
**Evidence:** `ui/feed.rs` — `if app.feed_scroll == 0 && total > visible_height {…}`.
**Fix direction:** Explicit `app.feed_follow: bool` (default true). Scroll up ⇒ false; `G`/`End` ⇒ true. Render: `if feed_follow { max_scroll } else { feed_scroll.min(max_scroll) }`.
**Verify:** Unit tests for default/scroll-up/G+End. Live: scroll up stops follow; bottom re-engages.

### P2-4 — Startup blocking snapshot before first paint
**Symptom:** `run()` awaited the initial `refresh_snapshot` before entering the terminal; slow API ⇒ blank shell, no feedback.
**Evidence:** `mod.rs` — initial snapshot block ran before `Terminal::new`/first draw.
**Fix direction:** Enter terminal + paint a "connecting to hub…" frame first, *then* await the snapshot. (Pre-flight ping stays before terminal setup — fail-fast on unreachable hub is correct.)
**Verify:** Slow/reachable API ⇒ alt-screen + status appears promptly.

---

## 2. Out of scope (do not do in this pass)
- Phase 5 detail overlays (`Enter`) and Phase 6 actions (`d` delegate) — separate work.
- Mouse support, themes, split panes, charts.
- Anything in the router / storage / query API. If a server-side change seems needed, **stop and report it**.
- **Drive-by fixes** for pre-existing issues noted by reviewers (see §6 minors) — record, don't fix, unless the parent directs.

## 3. Order & grouping (as executed)
- **Wave 1 (parallel, disjoint files):** P0-1 · P1-2 · P2-3.
- **Wave 2 (one implementer, coupled in `mod.rs`):** P0-2 + P1-1 + P2-1.
- **Wave 3 (serialized on `mod.rs`):** P2-2, then P1-3, then P2-4. *(In practice a single implementer landed all remaining items; each was still independently reviewed.)*
- **Reviewers:** one per item, read-only, parallel batches.

## 4. Live smoke test recipe (run at the end, on the GCP host)
```bash
export CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats
cargo build --bin hub-tui --features tui
nats-server -p 4222 --jetstream &                         # background
/data/cargo-targets/jfrie/nats/debug/hub-server --db-path /tmp/handoff_hub.db &
sleep 3
B=/data/cargo-targets/jfrie/nats/debug
$B/hub-register --identity worker-1 --capabilities code,review
$B/hub-publish --channel agents.tasks --from josh --message "hello"
# run hub-tui in a PTY; confirm: agents render, live msg in feed <1s,
# 'r' resets API age, wave counts show, 'q' exits code 0 with terminal intact.
# P2-2 check: kill nats-server, confirm footer shows "retry in Ns" with N growing
#   (1→2→4…→30 cap) on repeated failures; restart, confirm reconnect + delay clears.
```
Tear down: kill both servers, `rm -rf /tmp/handoff_hub.db`.

## 5. Report back (structured)
For each finding: `DONE` / `SKIPPED(reason)` + one line of evidence (test name or observed behavior) + **review verdict**. Then: files touched (final LOC), new warnings (should be zero), and anything noticed that isn't listed. Self-reports are untrusted — the parent re-runs the gate and the smoke test before landing.

---

## 6. Open findings & deferred items (triage here)

### ✅ P2-2 — RESOLVED by deletion (Josh's call, 2026-08-02)
**Root cause (smoke-test proven + async-nats 0.38 source):** the TUI watches connection health via `live_rx.recv() == None`, but async-nats auto-reconnects transparently, so `sub.next()` never returns `None` on an outage — it blocks, then resumes. The reconnect arm (`None =>` / originally `else`) was **dead at runtime**: the footer never showed `reconnecting`/`retry in Ns` even during a real server kill.

**Josh's decision:** the desired behavior is *"don't cry wolf over a blip; only speak up when an outage is long enough to actually disrupt work."* That behavior **already exists** — the refresh ticker's `ping` is wrapped in a 10s timeout (`query_api_client.rs:27`); a brief blip resolves before timeout (silent), a significant outage fails the ping → `snapshot_error` → footer `⚠` warning. So the P2-2 "reconnecting… retry in Ns" machinery was solving an already-solved problem and was dead code that would mislead future readers.

**Deleted (verified zero residue, gate green, 29 lib + 28 integration tests):**
- `mod.rs`: the `None =>` reconnect arm + `backoff` state + `next_backoff()` + its 2 tests + `NatsState` from the draw_now match; envelope arm simplified back to `Some(env) =>`. Pruned `error, warn` imports.
- `app.rs`: `reconnect_delay_secs` field + init.
- `chrome.rs`: the "retry in Ns" footer block + `Reconnecting`/`Disconnected` match arms (kept `Connected` → green header, which is live and truthful under auto-reconnect).
- `model.rs`: `ConnState::Reconnecting` + `Disconnected` variants (enum now `Connected` only, with a doc note explaining why).
- `event.rs`: `NatsState` (never sent) and `Quit` (also never sent — pre-existing dead variant flagged by the P2-1 reviewer) variants.
- `handler.rs`: the `NatsState` + `Quit` match arms + `test_nats_state_change` + unused `ConnState` test import.
- `nats_live.rs`: `note_dropped` (zero callers; drop accounting already in `App::ingest_envelope`) + unused `warn` import.

**What remains as the disconnect signal:** `ConnState::Connected` header (always green — truthful, since auto-reconnect keeps it up) + the footer `⚠` warning on a significant outage. This is the intended end state, not a gap.

### 🟡 Deferred minors (record, don't fix unless directed)
- **P2-3:** scrolling *down* to the bottom doesn't auto-re-engage follow — only `G`/`End` does. Handoff's required behavior is met via `G`/`End`; this is polish. A proper fix needs the handler to know `max_scroll` (currently render-only) — small design change, not a one-liner.
- **P2-4:** "connecting to hub…" is carried via `snapshot_error`, so it renders with the yellow `⚠` warning aesthetic — slightly misleading for a normal transient. Cosmetic; a dedicated neutral `status_message` field would be cleaner.
- **Pre-existing (not from this pass):** `AppEvent::Quit` variant is dead code (never sent); redundant `needs_redraw = true` on the first event (harmless).
