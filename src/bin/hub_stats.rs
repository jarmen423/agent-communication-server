//! hub-stats — observability queries against nats-hub's SurrealDB history.
//!
//! Usage:
//!   hub-stats --since 1h                       # overview: rate, hotspots, errors
//!   hub-stats --agent worker-1 --since 24h     # per-agent activity
//!   hub-stats --latency --channel agents.tasks --since 6h
//!   hub-stats --top-channels 10 --since 1h
//!   hub-stats --json                           # single JSON object for dashboards

use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use nats_hub::analytics::{Analytics, Interval, SurrealAnalytics, TimeRange};
use nats_hub::{Storage, SurrealStorage};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "hub-stats",
    about = "Observability queries over nats-hub message history"
)]
struct Args {
    /// Path to the SurrealDB database file
    #[arg(long, default_value = "nats_hub.db")]
    db_path: String,

    /// Lookback window: e.g. `30m`, `6h`, `7d` (default 1h)
    #[arg(long, default_value = "1h")]
    since: String,

    /// Show activity for a specific agent identity
    #[arg(long)]
    agent: Option<String>,

    /// Show reply-latency statistics (optionally with --channel)
    #[arg(long)]
    latency: bool,

    /// Restrict --latency to a single channel
    #[arg(long)]
    channel: Option<String>,

    /// Show top-N busiest channels
    #[arg(long)]
    top_channels: Option<usize>,

    /// Emit a single JSON object with all sections (for dashboards/pipes)
    #[arg(long)]
    json: bool,
}

fn parse_since(s: &str) -> i64 {
    let s = s.trim();
    let (num, unit) = match s.split_at(s.len().saturating_sub(1)) {
        (n, u) => (n, u),
    };
    let n: i64 = num.parse().unwrap_or(1);
    match unit {
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86_400,
        _ => s.parse().unwrap_or(3600), // bare number = seconds
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let storage = SurrealStorage::connect(&args.db_path).await?;
    storage.migrate().await?;
    storage.ping().await?;

    let analytics = SurrealAnalytics::new(Arc::new(storage) as Arc<dyn Storage>);
    let secs = parse_since(&args.since);
    let range = TimeRange::last(secs);

    if args.json {
        print_json(&analytics, &range, &args).await?;
        return Ok(());
    }

    if let Some(identity) = &args.agent {
        let activity = analytics.agent_activity(identity, &range).await?;
        println!("Agent activity: {identity} (last {secs}s)");
        println!("{:-<40}", "");
        println!("  sent:        {}", activity.sent);
        println!("  received DM: {}", activity.received_dm);
        println!("  events:      {}", activity.events);
        println!("  pending:     {}", activity.pending);
        return Ok(());
    }

    if args.latency {
        let stats = analytics
            .latency_stats(args.channel.as_deref(), &range)
            .await?;
        let scope = args
            .channel
            .as_deref()
            .map(|c| format!(" on {c}"))
            .unwrap_or_default();
        println!("Reply latency{scope} (last {secs}s)");
        println!("{:-<40}", "");
        println!("  samples: {}", stats.samples);
        if stats.samples > 0 {
            println!("  avg:    {:.1} ms", stats.avg_ms);
            println!("  min:    {:.1} ms", stats.min_ms);
            println!("  p50:    {:.1} ms", stats.p50_ms);
            println!("  p99:    {:.1} ms", stats.p99_ms);
            println!("  max:    {:.1} ms", stats.max_ms);
        }
        return Ok(());
    }

    if let Some(limit) = args.top_channels {
        let hotspots = analytics.channel_hotspots(&range, limit).await?;
        println!("Top {limit} channels by volume (last {secs}s)");
        println!("{:-<40}", "");
        if hotspots.is_empty() {
            println!("  (no messages)");
        } else {
            for (i, c) in hotspots.iter().enumerate() {
                println!(
                    "  {}. {:<28} {}",
                    i + 1,
                    truncate(&c.channel, 28),
                    c.messages
                );
            }
        }
        return Ok(());
    }

    // Default overview
    let rate = analytics.message_rate(&range, Interval::Hour).await?;
    let hotspots = analytics.channel_hotspots(&range, 5).await?;
    let errors = analytics.error_rate(&range, Interval::Hour).await?;

    let total: u64 = rate.iter().map(|d| d.count).sum();
    println!("Bus overview (last {secs}s)");
    println!("{:-<40}", "");
    println!("  total messages: {}", total);
    println!();
    println!("  message_rate (per hour bucket):");
    if rate.is_empty() {
        println!("    (no messages)");
    } else {
        for d in &rate {
            println!("    {}  {}", d.timestamp.format("%Y-%m-%d %H:%M"), d.count);
        }
    }
    println!();
    println!("  top channels:");
    if hotspots.is_empty() {
        println!("    (none)");
    } else {
        for c in &hotspots {
            println!("    {:<28} {}", truncate(&c.channel, 28), c.messages);
        }
    }
    println!();
    let err_total: u64 = errors.iter().map(|d| d.count).sum();
    println!("  error events: {}", err_total);

    Ok(())
}

async fn print_json(analytics: &SurrealAnalytics, range: &TimeRange, args: &Args) -> Result<()> {
    let mut obj = serde_json::Map::new();

    let rate = analytics.message_rate(range, Interval::Hour).await?;
    let hotspots = analytics.channel_hotspots(range, 10).await?;
    let errors = analytics.error_rate(range, Interval::Hour).await?;

    obj.insert(
        "message_rate".into(),
        serde_json::json!(rate
            .iter()
            .map(|d| serde_json::json!({
                "timestamp": d.timestamp.to_rfc3339(),
                "count": d.count
            }))
            .collect::<Vec<_>>()),
    );
    obj.insert(
        "top_channels".into(),
        serde_json::json!(hotspots
            .iter()
            .map(|c| serde_json::json!({ "channel": c.channel, "messages": c.messages }))
            .collect::<Vec<_>>()),
    );
    obj.insert(
        "error_rate".into(),
        serde_json::json!(errors
            .iter()
            .map(|d| serde_json::json!({
                "timestamp": d.timestamp.to_rfc3339(),
                "count": d.count
            }))
            .collect::<Vec<_>>()),
    );

    if let Some(identity) = &args.agent {
        let activity = analytics.agent_activity(identity, range).await?;
        obj.insert(
            "agent_activity".into(),
            serde_json::json!({
                "identity": activity.identity,
                "sent": activity.sent,
                "received_dm": activity.received_dm,
                "events": activity.events,
                "pending": activity.pending,
            }),
        );
    }

    if args.latency {
        let stats = analytics
            .latency_stats(args.channel.as_deref(), range)
            .await?;
        obj.insert(
            "latency".into(),
            serde_json::json!({
                "samples": stats.samples,
                "avg_ms": stats.avg_ms,
                "min_ms": stats.min_ms,
                "p50_ms": stats.p50_ms,
                "p99_ms": stats.p99_ms,
                "max_ms": stats.max_ms,
            }),
        );
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::Value::Object(obj))?
    );
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}
