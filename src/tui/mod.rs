//! TUI module — ratatui-based operations dashboard for nats-hub.
//!
//! Feature-gated behind `tui` (pulls ratatui + crossterm).
//! See `docs/archive/TUI_PLAN.md` for the full design.

pub mod api;
pub mod app;
pub mod event;
pub mod handler;
pub mod model;
pub mod nats_live;
pub mod ui;

use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::Event as CtEvent;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tracing::info;

use crate::client::HubClient;
use crate::query_api_client::ApiClient;

use app::App;
use event::AppEvent;
use model::PanelFocus;

/// TUI configuration from CLI args.
pub struct TuiOptions {
    pub nats_url: String,
    pub refresh_secs: u64,
    pub alive_secs: i64,
    pub feed_cap: usize,
}

/// Main entry point — sets up terminal, runs the event loop, restores on exit.
pub async fn run(opts: TuiOptions) -> Result<()> {
    // ── Pre-flight: ping the query API (fail fast, before terminal) ──
    let api = ApiClient::connect(&opts.nats_url)
        .await
        .context("connect to NATS for query API")?;
    api.request("ping", serde_json::json!({}))
        .await
        .context("hub-server query API ping failed — is hub-server running with --db-path?")?;
    info!("query API ping ok");

    // ── Connect live NATS client ─────────────────────────────
    let hub = HubClient::connect(&opts.nats_url, "hub-tui")
        .await
        .context("connect to NATS for live stream")?;
    let mut live_rx = nats_live::spawn_live_listener(&hub)
        .await
        .context("subscribe to channel.>")?;

    // ── Terminal setup (P2-4: before the blocking initial snapshot) ──
    enable_raw_mode()?;
    std::io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // ── App state ────────────────────────────────────────────
    let mut app = App::new(opts.feed_cap, opts.alive_secs);

    // ── Channels for unified event loop ──────────────────────
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    // P0-2: manual-refresh signal channel (run loop → ticker).
    let (refresh_tx, mut refresh_rx) = mpsc::unbounded_channel::<()>();

    // P2-4: paint one frame immediately so the alternate screen isn't blank
    // while we await the (possibly slow) initial snapshot.
    app.snapshot_error = Some("connecting to hub…".to_string());
    terminal.draw(|frame| ui::draw(frame, &app))?;

    // Initial snapshot (now non-blocking w.r.t. first paint).
    match api::refresh_snapshot(&api).await {
        Ok(snap) => app.apply_snapshot(snap),
        Err(e) => {
            app.snapshot_error = Some(e.to_string());
        }
    }

    // ── Background: API refresh ticker (P1-1 reuse one client) ──
    let tick_tx = tx.clone();
    let ticker_url = opts.nats_url.clone();
    let refresh_secs = opts.refresh_secs;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(refresh_secs));
        interval.tick().await; // first tick fires immediately; skip it
                               // P1-1: connect ONCE and reuse across ticks; reconnect lazily on error.
        let mut api: Option<ApiClient> = ApiClient::connect(&ticker_url).await.ok();
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = refresh_rx.recv() => {} // P0-2: manual `r` refresh
            }
            // Ensure a live client exists (lazy reconnect).
            if api.is_none() {
                match ApiClient::connect(&ticker_url).await {
                    Ok(a) => api = Some(a),
                    Err(e) => {
                        let _ = tick_tx.send(AppEvent::SnapshotError(format!("connect: {e}")));
                        continue;
                    }
                }
            }
            let result = api::refresh_snapshot(api.as_ref().expect("api present")).await;
            match result {
                Ok(snap) => {
                    let _ = tick_tx.send(AppEvent::Snapshot(snap));
                }
                Err(e) => {
                    let _ = tick_tx.send(AppEvent::SnapshotError(e.to_string()));
                    api = None; // drop dead client; reconnect next tick
                }
            }
        }
    });

    // ── Background: terminal input reader ────────────────────
    let key_tx = tx.clone();
    tokio::spawn(async move {
        loop {
            // Poll with timeout so we don't block forever (allows Tick)
            if crossterm::event::poll(Duration::from_millis(250)).unwrap_or(false) {
                match crossterm::event::read() {
                    Ok(CtEvent::Key(key)) => {
                        if key_tx.send(AppEvent::Key(key)).is_err() {
                            break;
                        }
                    }
                    Ok(CtEvent::Resize(w, h)) => {
                        if key_tx.send(AppEvent::Resize(w, h)).is_err() {
                            break;
                        }
                    }
                    _ => {}
                }
            } else {
                // Idle tick
                if key_tx.send(AppEvent::Tick).is_err() {
                    break;
                }
            }
        }
    });

    // ── Main event loop ──────────────────────────────────────
    let result = run_loop(
        &mut terminal,
        &mut app,
        &mut rx,
        &mut live_rx,
        &refresh_tx,
        &tx,
        &opts,
    )
    .await;

    // ── Restore terminal ─────────────────────────────────────
    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;

    // Drain NATS
    let _ = hub.drain().await;

    result
}

#[allow(clippy::too_many_arguments)]
async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    rx: &mut mpsc::UnboundedReceiver<AppEvent>,
    live_rx: &mut mpsc::UnboundedReceiver<crate::protocol::Envelope>,
    refresh_tx: &mpsc::UnboundedSender<()>,
    tx: &mpsc::UnboundedSender<AppEvent>,
    opts: &TuiOptions,
) -> Result<()> {
    // Draw once up front.
    terminal.draw(|frame| ui::draw(frame, app))?;

    // P2-1: frame coalescing — only redraw on Tick when something changed.
    let mut needs_redraw = true;
    // P1-3: last wave we lazily fetched task counts for.
    let mut last_wave_fetched: Option<String> = None;

    loop {
        tokio::select! {
            // Unified event channel (keys, ticks, snapshots)
            Some(ev) = rx.recv() => {
                // P2-1: decide redraw policy by event type. Envelopes are
                // coalesced into the next Tick; user-visible events draw now.
                let draw_now = matches!(
                    ev,
                    AppEvent::Key(_)
                        | AppEvent::Snapshot(_)
                        | AppEvent::SnapshotError(_)
                        | AppEvent::WaveTasks { .. }
                        | AppEvent::Resize(_, _)
                );
                let is_tick = matches!(ev, AppEvent::Tick);

                handler::apply_event(app, ev);

                // P0-2: drain the refresh flag and signal the ticker.
                if app.refresh_requested {
                    app.refresh_requested = false;
                    let _ = refresh_tx.send(());
                }

                // P1-3: lazy wave task fetch on selection change.
                maybe_fetch_wave(app, tx, &mut last_wave_fetched, &opts.nats_url);

                if draw_now || (is_tick && needs_redraw) {
                    terminal.draw(|frame| ui::draw(frame, app))?;
                    needs_redraw = false;
                }

                if app.should_quit {
                    break;
                }
            }

            // Live NATS envelopes — coalesced, no inline draw (P2-1).
            //
            // async-nats auto-reconnects transparently, so this stream does
            // not end on a server outage — it just pauses, then resumes. A
            // significant outage surfaces instead via the refresh ticker's
            // failed `ping` (footer ⚠ warning), which is the user-visible
            // signal we actually want. No manual reconnect handling here.
            Some(env) = live_rx.recv() => {
                handler::apply_event(app, AppEvent::Envelope(Box::new(env)));
                needs_redraw = true;
            }

            // Safety net: only reachable if EVERY branch is disabled (rx and
            // live_rx both closed). Clean shutdown guard.
            else => {
                break;
            }
        }
    }

    Ok(())
}

/// P1-3: when the Waves panel is focused and the selected wave changes,
/// spawn a task to fetch its task counts and deliver them as `WaveTasks`.
/// Handler stays pure — the async I/O lives here, not in `handler`.
fn maybe_fetch_wave(
    app: &App,
    tx: &mpsc::UnboundedSender<AppEvent>,
    last: &mut Option<String>,
    nats_url: &str,
) {
    if app.focus != PanelFocus::Waves {
        return;
    }
    let Some(row) = app.waves.get(app.wave_sel) else {
        return;
    };
    let wave_id = row.wave_id.clone();
    if last.as_deref() == Some(wave_id.as_str()) {
        return;
    }
    *last = Some(wave_id.clone());

    let tx = tx.clone();
    let url = nats_url.to_string();
    tokio::spawn(async move {
        let Ok(api) = ApiClient::connect(&url).await else {
            return;
        };
        if let Ok((done, total)) = api::fetch_wave_task(&api, &wave_id).await {
            let _ = tx.send(AppEvent::WaveTasks {
                wave_id,
                done,
                total,
            });
        }
    });
}
