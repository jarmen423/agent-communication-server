//! hub-stats — observability queries via the query API.
//!
//! Usage:
//!   hub-stats --since 1h                       # overview: rate, hotspots, errors
//!   hub-stats --agent worker-1 --since 24h     # per-agent activity
//!   hub-stats --latency --channel agents.tasks --since 6h
//!   hub-stats --top-channels 10 --since 1h
//!   hub-stats --json                           # single JSON object for dashboards

use anyhow::Result;
use clap::Parser;
use nats_hub::ApiClient;
use serde_json::Value;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-stats", about = "Observability queries via query API")]
struct Args {
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
    #[arg(long, default_value = "1h")] since: String,
    #[arg(long)] agent: Option<String>,
    #[arg(long)] latency: bool,
    #[arg(long)] channel: Option<String>,
    #[arg(long)] top_channels: Option<usize>,
    #[arg(long)] json: bool,
}

fn parse_since(s: &str) -> i64 {
    let s = s.trim();
    let (num, unit) = match s.split_at(s.len().saturating_sub(1)) { (n, u) => (n, u) };
    let n: i64 = num.parse().unwrap_or(1);
    match unit { "m" => n * 60, "h" => n * 3600, "d" => n * 86_400, _ => s.parse().unwrap_or(3600) }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();
    let args = Args::parse();
    let api = ApiClient::connect(&args.nats_url).await?;
    let secs = parse_since(&args.since);

    if args.json { return print_json(&api, &args, secs).await; }

    if let Some(identity) = &args.agent {
        let resp = api.request("stats.agent_activity", serde_json::json!({"identity": identity, "secs": secs})).await?;
        let activity = resp.get("activity").cloned().unwrap_or(Value::Null);
        println!("Agent activity: {identity} (last {secs}s)\n{:-<40}", "");
        println!("  sent:        {}", activity.get("sent").and_then(|v| v.as_u64()).unwrap_or(0));
        println!("  received DM: {}", activity.get("received_dm").and_then(|v| v.as_u64()).unwrap_or(0));
        println!("  events:      {}", activity.get("events").and_then(|v| v.as_u64()).unwrap_or(0));
        println!("  pending:     {}", activity.get("pending").and_then(|v| v.as_u64()).unwrap_or(0));
        return Ok(());
    }

    if args.latency {
        let mut params = serde_json::json!({"secs": secs});
        if let Some(ch) = &args.channel { params["channel"] = serde_json::json!(ch); }
        let resp = api.request("stats.latency", params).await?;
        let stats = resp.get("stats").cloned().unwrap_or(Value::Null);
        let scope = args.channel.as_deref().map(|c| format!(" on {c}")).unwrap_or_default();
        println!("Reply latency{scope} (last {secs}s)\n{:-<40}", "");
        let samples = stats.get("samples").and_then(|v| v.as_u64()).unwrap_or(0);
        println!("  samples: {samples}");
        if samples > 0 {
            println!("  avg:    {:.1} ms", stats.get("avg_ms").and_then(|v| v.as_f64()).unwrap_or(0.0));
            println!("  min:    {:.1} ms", stats.get("min_ms").and_then(|v| v.as_f64()).unwrap_or(0.0));
            println!("  p50:    {:.1} ms", stats.get("p50_ms").and_then(|v| v.as_f64()).unwrap_or(0.0));
            println!("  p99:    {:.1} ms", stats.get("p99_ms").and_then(|v| v.as_f64()).unwrap_or(0.0));
            println!("  max:    {:.1} ms", stats.get("max_ms").and_then(|v| v.as_f64()).unwrap_or(0.0));
        }
        return Ok(());
    }

    if let Some(limit) = args.top_channels {
        let resp = api.request("stats.channel_hotspots", serde_json::json!({"secs": secs, "limit": limit})).await?;
        let hotspots = resp.get("hotspots").and_then(|h| h.as_array().cloned()).unwrap_or_default();
        println!("Top {limit} channels by volume (last {secs}s)\n{:-<40}", "");
        if hotspots.is_empty() { println!("  (no messages)"); }
        else {
            for (i, c) in hotspots.iter().enumerate() {
                println!("  {}. {:<28} {}", i + 1, truncate(c.get("channel").and_then(|v| v.as_str()).unwrap_or("?"), 28),
                    c.get("messages").and_then(|v| v.as_u64()).unwrap_or(0));
            }
        }
        return Ok(());
    }

    // Default overview
    let rate_resp = api.request("stats.message_rate", serde_json::json!({"secs": secs, "interval": "hour"})).await?;
    let hotspots_resp = api.request("stats.channel_hotspots", serde_json::json!({"secs": secs, "limit": 5})).await?;
    let errors_resp = api.request("stats.error_rate", serde_json::json!({"secs": secs, "interval": "hour"})).await?;

    let rate = rate_resp.get("data").and_then(|d| d.as_array().cloned()).unwrap_or_default();
    let hotspots = hotspots_resp.get("hotspots").and_then(|h| h.as_array().cloned()).unwrap_or_default();
    let errors = errors_resp.get("data").and_then(|d| d.as_array().cloned()).unwrap_or_default();

    let total: u64 = rate.iter().map(|d| d.get("count").and_then(|v| v.as_u64()).unwrap_or(0)).sum();
    println!("Bus overview (last {secs}s)\n{:-<40}", "");
    println!("  total messages: {total}\n");
    println!("  message_rate (per hour bucket):");
    if rate.is_empty() { println!("    (no messages)"); }
    else {
        for d in &rate {
            let ts = d.get("timestamp").and_then(|v| v.as_str()).unwrap_or("?");
            let cnt = d.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
            println!("    {ts}  {cnt}");
        }
    }
    println!("\n  top channels:");
    if hotspots.is_empty() { println!("    (none)"); }
    else {
        for c in &hotspots {
            println!("    {:<28} {}", truncate(c.get("channel").and_then(|v| v.as_str()).unwrap_or("?"), 28),
                c.get("messages").and_then(|v| v.as_u64()).unwrap_or(0));
        }
    }
    let err_total: u64 = errors.iter().map(|d| d.get("count").and_then(|v| v.as_u64()).unwrap_or(0)).sum();
    println!("\n  error events: {err_total}");
    Ok(())
}

async fn print_json(api: &ApiClient, args: &Args, secs: i64) -> Result<()> {
    let mut obj = serde_json::Map::new();
    let rate = api.request("stats.message_rate", serde_json::json!({"secs": secs, "interval": "hour"})).await?;
    let hotspots = api.request("stats.channel_hotspots", serde_json::json!({"secs": secs, "limit": 10})).await?;
    let errors = api.request("stats.error_rate", serde_json::json!({"secs": secs, "interval": "hour"})).await?;
    obj.insert("message_rate".into(), rate.get("data").cloned().unwrap_or(Value::Null));
    obj.insert("top_channels".into(), hotspots.get("hotspots").cloned().unwrap_or(Value::Null));
    obj.insert("error_rate".into(), errors.get("data").cloned().unwrap_or(Value::Null));
    if let Some(identity) = &args.agent {
        let resp = api.request("stats.agent_activity", serde_json::json!({"identity": identity, "secs": secs})).await?;
        obj.insert("agent_activity".into(), resp.get("activity").cloned().unwrap_or(Value::Null));
    }
    if args.latency {
        let mut params = serde_json::json!({"secs": secs});
        if let Some(ch) = &args.channel { params["channel"] = serde_json::json!(ch); }
        let resp = api.request("stats.latency", params).await?;
        obj.insert("latency".into(), resp.get("stats").cloned().unwrap_or(Value::Null));
    }
    println!("{}", serde_json::to_string_pretty(&Value::Object(obj))?);
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() }
    else { format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>()) }
}
