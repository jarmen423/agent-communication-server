# hub-tui — Implementation Plan

**Status:** Plan only (no code yet)  
**Goal:** A ratatui-based **clean, modern daily driver** for nats-hub — keyboard-driven operations dashboard, not the arcade WebSocket visualizer.

**Prerequisites:** `nats-server` + `hub-server` running with `--db-path` (query API requires persisted storage). `--ws-addr` is **not** required.

**Build gate:** `export CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats` (see `AGENTS.md`).

---

## 1. Architecture

### 1.1 Data planes (two consumers, one NATS URL)

```
┌─────────────────────────────────────────────────────────────────┐
│                         hub-tui process                          │
│  ┌──────────────────┐              ┌──────────────────────────┐ │
│  │ HubClient        │              │ ApiClient                 │ │
│  │ (live, push)     │              │ (snapshot, pull/refresh)  │ │
│  └────────┬─────────┘              └────────────┬─────────────┘ │
│           │ subscribe                           │ request-reply   │
└───────────┼───────────────────────────────────┼─────────────────┘
            │                                     │
            ▼                                     ▼
     channel.>                            hub.api.<operation>
     (routed envelopes)                  (JSON ApiRequest/Response)
            │                                     │
            └──────────────┬──────────────────────┘
                           ▼
                    hub-server + NATS
```

| Plane | Library API | NATS pattern | Purpose |
|-------|-------------|--------------|---------|
| **Live** | `HubClient::subscribe_all()` → `channel.>` | Wildcard subscription on **post-router** subjects | Message feed, infer per-agent activity from `Envelope` traffic (`MessageKind::Event`, `Status`, etc.) |
| **Persistent** | `ApiClient::request(op, params)` | `hub.api.<op>` request-reply | Agent registry, sessions, waves, analytics — same path as `hub-agents`, `hub-session`, `hub-wave`, `hub-stats` |

**Why not WebSocket?** Phase 7 visualizer uses `ws_bridge` + browser. The TUI talks NATS directly (lighter, no HTTP server dependency, works headless over SSH).

**Why not direct SurrealDB?** RocksDB single-writer lock — CLIs must use `ApiClient` while `hub-server` holds the DB (`docs/DB_ACCESS_PROBLEM.md`, `query_api_client.rs`).

### 1.2 Runtime model (tokio + ratatui)

Single `#[tokio::main]` binary with a **unified event loop** using `tokio::select!`:

1. **NATS live stream** — background task spawned at startup: `HubClient::connect` → `subscribe_all()` → push decoded `Envelope`s into `mpsc::UnboundedSender<Envelope>` (same pattern as `client.rs::subscribe_subject`).
2. **API refresh ticker** — `tokio::time::interval` (default **5s**, configurable `--refresh-secs`): batch `ApiClient` calls off the UI hot path; results update `App` state via another channel or `Arc<tokio::sync::RwLock<App>>`.
3. **Terminal input** — dedicated task or integrated poll: `crossterm::event::read()` with **timeout** aligned to render tick (e.g. 250ms) so the loop stays responsive without blocking NATS.
4. **Render tick** — redraw when state changes or on idle tick (cap at ~4 FPS when quiescent to save CPU).

On startup:

- `ApiClient::request("ping", {})` — fail fast with clear stderr if `hub-server` query API is down.
- Optional: `agent.find` with `alive_within_secs: 120` for initial agent table.

**Connection identity:** `HubClient::connect(nats_url, "hub-tui")` — observability only; does not register as a worker unless we add optional `--register` later.

### 1.3 Live agent status (merge DB + stream)

Persisted **`AgentRecord`** (`identity`, `capabilities`, `last_seen`) comes from `agent.find`.

**Derived status** (in-memory map `agent_id → AgentLiveState`):

| Signal | Source | Mapping |
|--------|--------|---------|
| `last_seen` age | API refresh | `>120s` → **offline**, `≤120s` → **idle** (configurable `--alive-secs`) |
| Recent `MessageKind::Event` with `event_type` | `channel.>` | `started` → **working**, `completed` → **idle**, `error` → **error** |
| `MessageKind::Status` payload | `channel.>` | Use `payload.status` when present (`thinking`, `working`, …) |
| Heartbeat | *Optional v2* | Subscribe `hub.presence` (not on `channel.>`) to tighten liveness without waiting for API poll |

Reuse **`event_summary`** / **`event_type`** from `nats_hub::events` for feed lines (same semantics as `hub-watch` / `format_event_line`, but render via ratatui `Span` styles instead of ANSI escape strings).

### 1.4 Error / disconnect behavior

- NATS disconnect: show **banner** in status bar; exponential backoff reconnect in background task.
- Query API timeout: keep last good snapshot; show `API stale (12s)` in footer.
- Malformed JSON on `channel.>`: drop + increment `feed_dropped` counter (log at `warn` via `tracing`).

### 1.5 Feature flags & binary registration

- **`[[bin]] name = "hub-tui"`** with `required-features = ["storage-surreal"]` — same as `hub-agents` / `hub-session` (typed `AgentRecord`, `SessionRecord`, `WaveRecord` from `nats_hub::storage`).
- TUI does **not** link SurrealDB at runtime; it only needs serde types + `ApiClient` + `HubClient`.

---

## 2. Screen layout

### 2.1 Default view: **Dashboard** (single screen, no modal stack in v1)

Ratatui root: `Layout::vertical` with **header / body / footer**.

```
┌─ nats-hub ───────────────────────────── NATS ok │ API 2s │ msgs 1.2k/s ─┐
│ ┌ Agents ──────────┐ ┌ Sessions ────────┐ ┌ Waves ───────────────────┐ │
│ │● worker-1  work   │ │ a3f7  active     │ │ wave-01  running  2/5    │ │
│ │○ cursor-1 idle   │ │ b91c  active     │ │ wave-02  merged   5/5    │ │
│ │✗ agy-1   error   │ │                  │ │                          │ │
│ │  (j/k, Enter)    │ │  (j/k, Enter)    │ │  (j/k, Enter)            │ │
│ └──────────────────┘ └──────────────────┘ └──────────────────────────┘ │
│ ┌ Message feed ────────────────────────────────────────────────────────┐ │
│ │ 14:02:01  event     worker-1  session.abc  "implement handler"       │ │
│ │ 14:02:03  message   josh       agents.tasks  {"prompt":"..."}         │ │
│ │ 14:02:05  status    worker-1   system        working                  │ │
│ │  (PgUp/Dn, / filter, f focus kinds)                                  │ │
│ └──────────────────────────────────────────────────────────────────────┘ │
│ [Tab] panel  [r] refresh  [/?] help  [q] quit                            │
└──────────────────────────────────────────────────────────────────────────┘
```

**Proportions (ratatui `Constraint`):**

- Header: `Length(1)`
- Body top row (3 columns): `Ratio(1,1,1)` with `Min(8)` height
- Feed: `Min(10)` + `Percentage(50)` of remaining
- Footer: `Length(1)`

**Widgets:**

| Panel | Widget | Data |
|-------|--------|------|
| Agents | `Table` or `List` with stateful selection | `Vec<AgentRow>` from API + live map |
| Sessions | `Table` | `session.list` filter `status: "active"` (and recent `closed` optional `--all-sessions`) |
| Waves | `Table` | `wave.list` + on row select lazy `wave.list_tasks` for `done/total` counts |
| Feed | `List` (virtualized: keep last **500** lines in `VecDeque`) | All envelopes from `channel.>`; default filter hides noisy kinds optional |
| Header/Footer | `Paragraph` | Connection, refresh age, compact `stats.message_rate` (last 5m) |

**Color palette (modern, not arcade):** dark background (`Color::Rgb(24,24,27)`), muted borders (`Rgb(63,63,70)`), semantic accents: green=ok/working, yellow=progress, red=error, cyan=event, gray=metadata. No animations, no particles.

### 2.2 Focus model

- **Four focus targets:** `Agents` | `Sessions` | `Waves` | `Feed`
- `Tab` / `Shift+Tab` cycles; number keys `1`–`4` jump.
- Arrow keys / `j` `k` move selection within focused panel.
- `Enter` on Agents/Sessions/Waves opens **detail overlay** (Phase 2 — see §4); v1 can log to footer or no-op.

### 2.3 Keybindings (v1)

| Key | Action |
|-----|--------|
| `q`, `Ctrl+C` | Quit (drain NATS, restore terminal) |
| `Tab` / `Shift+Tab` | Next / previous panel focus |
| `1`–`4` | Focus Agents / Sessions / Waves / Feed |
| `j` / `k` or `↑` / `↓` | Move selection |
| `r` | Force API refresh now |
| `/` | Open feed filter prompt (substring on from/channel/summary) |
| `Esc` | Clear filter / close overlay |
| `?` | Toggle help popup (key legend) |
| `PgUp` / `PgDn` | Feed scroll |

**Out of scope v1:** mouse, vim splits, sending messages (use `hub-delegate` / `hub-publish` in another terminal). Phase 3 may add `d` delegate prompt using `HubClient::send_to`.

### 2.4 Feed line format

Compact single line per envelope (width-aware truncate):

```
{HH:MM:SS}  {kind:8}  {from:16}  {channel:20}  "{summary}"
```

- `MessageKind::Event` → use `event_summary(env)`
- Others → short JSON or `payload` keys (`message`, `status`, …)

Optional toggles (later): `e` events only, `a` all kinds.

---

## 3. Module structure

Keep each file **&lt; 400 LOC** (project convention). TUI lives in the library crate so widgets are testable without a terminal.

```
src/
├── bin/
│   └── hub_tui.rs          # clap Args, tracing init, run_app(), < ~120 LOC
└── tui/
    ├── mod.rs              # pub mod app, event, ui, api, model; re-exports
    ├── app.rs              # App struct, focus, selection indices, merge logic
    ├── model.rs            # AgentRow, FeedLine, PanelFocus, AgentLiveState
    ├── api.rs              # refresh_snapshot(api) -> Snapshot; typed wrappers
    ├── nats_live.rs        # spawn_live_listener(HubClient) -> UnboundedReceiver
    ├── event.rs            # AppEvent enum: NatEnvelope, ApiSnapshot, Tick, Key(KeyEvent)
    ├── handler.rs          # apply_event(&mut App, AppEvent)
    └── ui/
        ├── mod.rs          # draw(frame, &App)
        ├── layout.rs       # dashboard chunks
        ├── agents.rs       # render_agents_table
        ├── sessions.rs
        ├── waves.rs
        ├── feed.rs
        └── chrome.rs       # header, footer, help popup
```

**`hub_tui.rs` responsibilities only:**

- Parse `--nats-url` (default `nats://127.0.0.1:4222`), `--refresh-secs`, `--alive-secs`, `--feed-cap`
- `tracing_subscriber` with `RUST_LOG` default `warn,nats_hub=info`
- `enable_raw_mode`, `EnterAlternateScreen`, `run_app`, `disable_raw_mode` on exit
- Delegate to `tui::run(nats_url, options).await`

**`api.rs` — single refresh function** (parallelize with `tokio::join!`):

```rust
// Pseudocode — one place to update when query API adds ops
async fn refresh_snapshot(api: &ApiClient) -> Result<Snapshot> {
    let (ping, agents, sessions, waves, rate) = tokio::join!(
        api.request("ping", json!({})),
        api.request("agent.find", json!(AgentFilter::default().limit(200))),
        api.request("session.list", json!(SessionFilter::new().status("active").limit(50))),
        api.request("wave.list", json!({})),  // optional status filter
        api.request("stats.message_rate", json!({"secs": 300, "interval_secs": 60})),
    );
    // deserialize into Snapshot
}
```

**`nats_live.rs`:** thin wrapper over `HubClient::subscribe_all()` — no duplicate subscribe logic.

**Tests (no terminal):**

- `model.rs`: status merge rules (unit tests)
- `handler.rs`: feed ring buffer cap, filter
- `api.rs`: mock `ApiClient` not needed v1; optional JSON fixture deserialize tests

**`lib.rs` change:** `pub mod tui;` gated behind optional feature `tui` **or** always compile `tui` module but only build binary with deps — recommend **`feature "tui" = ["dep:ratatui", "dep:crossterm"]`** so library consumers do not pull TUI deps by default.

---

## 4. Implementation phases

### Phase 1 — Skeleton & wiring (MVP shell)

- Add dependencies + `hub-tui` binary + `tui` feature.
- `hub_tui.rs`: terminal setup, empty `Paragraph` "connecting…", clean exit.
- Connect `ApiClient` + `ping`; connect `HubClient` + `subscribe_all`.
- Unified loop: quit key, NATS messages counted but not displayed.
- **Done when:** `CARGO_TARGET_DIR=... cargo run --bin hub-tui` runs against live stack without panic.

### Phase 2 — Dashboard panels (read-only)

- Implement `Snapshot` refresh on interval + manual `r`.
- Render Agents, Sessions, Waves tables from API.
- Focus + selection navigation.
- Footer: last refresh time, agent count.

### Phase 3 — Live message feed

- `VecDeque<FeedLine>` with cap; ingest all `channel.>` envelopes.
- Feed panel scroll + `/` filter.
- Merge live events into `AgentLiveState` for status column.

### Phase 4 — Stats chrome & polish

- Header: `stats.message_rate` sparkline or numeric msgs/min.
- Stale/error banners; NATS reconnect.
- Help overlay `?`; document keys in `docs/TUI_PLAN.md` (this file) + one-line in `README.md`.

### Phase 5 — Detail views (stretch)

- `Enter` on session → `session.get` + recent `history.query` for `channel.session.<id>`.
- `Enter` on wave → `wave.get` + `wave.list_tasks` sub-table overlay.
- `Enter` on agent → `agent.get` + `stats.agent_activity` for 1h.

### Phase 6 — Actions (stretch, post–daily-driver)

- `d` quick-delegate: modal → `HubClient::send_to` + subscribe task channel (mirror `hub-delegate` subset).
- `w` watch filter: set feed filter to `session.<id>` / `wave.<id>` channel prefix.

**Explicitly defer:** ratatui charts crate, mouse, split panes, themes, IRC-style command mode.

---

## 5. Dependencies

Add to `Cargo.toml`:

```toml
[features]
default = ["storage-surreal"]
storage-surreal = ["dep:surrealdb"]
tui = ["dep:ratatui", "dep:crossterm"]

[dependencies]
ratatui = { version = "0.29", optional = true }
crossterm = { version = "0.28", optional = true }

[[bin]]
name = "hub-tui"
path = "src/bin/hub_tui.rs"
required-features = ["storage-surreal", "tui"]
```

**Versions:** Pin minor versions compatible with Rust 2021 / `rust-version = "1.74"`. `ratatui` 0.29 + `crossterm` 0.28 is the standard pair (no `tui` legacy crate).

**Already used (no new dep):** `tokio`, `async-nats`, `serde_json`, `anyhow`, `clap`, `tracing`, `chrono`, `nats_hub::{HubClient, ApiClient, …}`.

**Not needed:** `tokio-tungstenite`, `prometheus` crate, `hub-server --ws-addr`.

---

## 6. CLI interface

```text
hub-tui — nats-hub terminal dashboard

USAGE:
    hub-tui [OPTIONS]

OPTIONS:
    --nats-url <URL>       NATS server [default: nats://127.0.0.1:4222]
    --refresh-secs <N>     Query API poll interval [default: 5]
    --alive-secs <N>       Agent "online" threshold from last_seen [default: 120]
    --feed-cap <N>         Max feed lines retained [default: 500]
    -h, --help
```

**Run recipe:**

```bash
export CARGO_TARGET_DIR=/data/cargo-targets/jfrie/nats
nats-server -p 4222 --jetstream   # if not already up
/data/cargo-targets/jfrie/nats/debug/hub-server --db-path /path/to/nats_hub.db
cargo build --bin hub-tui --features tui
/data/cargo-targets/jfrie/nats/debug/hub-tui
```

---

## 7. Relationship to other surfaces

| Surface | Role | Transport |
|---------|------|-----------|
| **hub-tui** | Daily driver: lists, feed, keyboard | NATS `channel.>` + `hub.api.>` |
| **Visualizer** (`visualizer/`) | Delight / demo arcade view | WebSocket via `--ws-addr` |
| **hub-watch** | CLI one-off event tail | NATS filtered subscribe |
| **hub-stats** | CLI analytics dump | Query API only |

Same underlying truth: router mirrors to DB; live traffic appears on `channel.*` after routing.

---

## 8. Risks & mitigations

| Risk | Mitigation |
|------|------------|
| High `channel.>` volume fills memory/CPU | Ring buffer cap; optional kind filter; sample or drop `stdout` events in feed |
| `ratatui` + `tokio` threading | Single task owns `Terminal`; only `App` state shared via messages |
| Query API load from 5s poll | Batch ops in one tick; backoff when errors; manual `r` only when needed |
| Terminal resize | Handle `Event::Resize` → `terminal.clear()` / redraw |
| SSH latency | Keep refresh modest; no full-screen flicker (diff-friendly widgets) |

---

## 9. Acceptance criteria (v1 complete)

1. With `hub-server` running, TUI shows **≥1** agent row from `agent.find` and updates **last_seen** on refresh.
2. Publishing via `hub-publish` or worker traffic appears in **feed within 1s**.
3. Active session created via `hub-session` appears in Sessions panel after refresh.
4. Active wave appears in Waves panel; task counts correct after `wave.list_tasks`.
5. `q` exits without leaving terminal corrupted.
6. All new `src/tui/**` files ≤ 400 LOC; `cargo fmt`, `cargo test`, `cargo build --bin hub-tui` pass with `CARGO_TARGET_DIR` set.

---

*Aligned with `AGENTS.md`, `docs/PRODUCT_VISION.md` (observable agent layer), and `nats-hub-development` skill (query API, no direct DB, build gate).*