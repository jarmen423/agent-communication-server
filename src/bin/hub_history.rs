//! hub-history — query message history via the query API.
//!
//! Routes DB operations through hub-server's query API (NATS request-reply)
//! to avoid RocksDB single-writer lock contention.
//!
//! Usage:
//!   hub-history                          # all recent messages (default limit 20)
//!   hub-history --channel agents.tasks    # filter by channel
//!   hub-history --from agent-alpha         # filter by sender
//!   hub-history --tail                    # live follow (subscribe to all channels)

use anyhow::Result;
use clap::Parser;
use nats_hub::{ApiClient, HistoryQuery};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-history", about = "Query message history via query API")]
struct Args {
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
    #[arg(long)] channel: Option<String>,
    #[arg(long)] from: Option<String>,
    #[arg(long)] kind: Option<String>,
    #[arg(long, default_value = "20")] limit: usize,
    #[arg(long)] tail: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();
    let args = Args::parse();

    let api = ApiClient::connect(&args.nats_url).await?;

    let mut query = HistoryQuery::new().limit(args.limit);
    if let Some(ref ch) = args.channel { query = query.channel(ch.clone()); }
    if let Some(ref from) = args.from { query = query.from(from.clone()); }
    if let Some(ref kind) = args.kind { query = query.kind(kind.clone()); }

    let resp = api.request("history.query", serde_json::to_value(&query)?).await?;
    let results: Vec<nats_hub::storage::EnvelopeRecord> = resp.get("envelopes")
        .and_then(|e| serde_json::from_value(e.clone()).ok()).unwrap_or_default();

    if results.is_empty() {
        println!("(no messages found)");
    } else {
        println!("{:<36} {:<7} {:<20} {:<15} {:<8} {}", "ID", "KIND", "FROM", "CHANNEL", "TIME", "PAYLOAD");
        println!("{}", "-".repeat(120));
        for record in &results {
            let time = record.timestamp.format("%H:%M:%S").to_string();
            let payload_preview = preview_payload(&record.payload, 60);
            println!("{:<36} {:<7} {:<20} {:<15} {:<8} {}",
                &record.id.chars().take(36).collect::<String>(),
                record.kind, truncate_str(&record.from_identity, 20),
                truncate_str(&record.channel, 15), time, payload_preview);
        }
        println!("\n({} message(s))", results.len());
    }

    if args.tail {
        println!("\n--- live tail (Ctrl+C to stop) ---");
        let client = nats_hub::HubClient::connect(&args.nats_url, "hub-history").await?;
        let mut rx = client.subscribe_all().await?;
        while let Some(env) = rx.recv().await {
            let time = env.meta.timestamp.format("%H:%M:%S").to_string();
            let payload_preview = preview_payload(&env.payload, 80);
            println!("{:<7} {:<20} {:<15} {:<8} {}",
                format!("{:?}", env.meta.kind).to_lowercase(),
                truncate_str(&env.meta.from, 20), truncate_str(&env.meta.channel, 15),
                time, payload_preview);
        }
    }
    Ok(())
}

fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() > max { &s[..max.saturating_sub(2)] } else { s }
}

fn preview_payload(payload: &serde_json::Value, max: usize) -> String {
    let s = if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
        text.to_string()
    } else if let Some(result) = payload.get("result").and_then(|v| v.as_str()) {
        result.to_string()
    } else if let Some(status) = payload.get("status").and_then(|v| v.as_str()) {
        format!("status: {}", status)
    } else if let Some(error) = payload.get("error").and_then(|v| v.as_str()) {
        format!("error: {}", error)
    } else {
        payload.to_string()
    };
    if s.len() > max { format!("{}...", &s[..max.saturating_sub(3)]) } else { s }
}
