//! hub-tui — ratatui-based operations dashboard for nats-hub.
//!
//! Keyboard-driven daily driver: agents, sessions, waves, live feed.
//! See `docs/archive/TUI_PLAN.md` for the full design.

use anyhow::Result;
use clap::Parser;

/// nats-hub terminal dashboard.
#[derive(Parser)]
#[command(name = "hub-tui", about = "nats-hub terminal dashboard")]
struct Args {
    /// NATS server URL.
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,

    /// Query API poll interval in seconds.
    #[arg(long, default_value_t = 5)]
    refresh_secs: u64,

    /// Agent "online" threshold from last_seen (seconds).
    #[arg(long, default_value_t = 120)]
    alive_secs: i64,

    /// Max feed lines retained.
    #[arg(long, default_value_t = 500)]
    feed_cap: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Tracing: default warn, nats_hub at info
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,nats_hub=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    // Panic hook: best-effort restore the terminal so the user's shell is
    // usable after a crash, then delegate to the default hook for backtraces.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
        default_hook(info);
    }));

    nats_hub::tui::run(nats_hub::tui::TuiOptions {
        nats_url: args.nats_url,
        refresh_secs: args.refresh_secs,
        alive_secs: args.alive_secs,
        feed_cap: args.feed_cap,
    })
    .await
}
