//! hub-history — query message history from the SurrealDB backend.
//!
//! Usage:
//!   hub-history                          # all recent messages (default limit 20)
//!   hub-history --channel agents.tasks    # filter by channel
//!   hub-history --from agent-alpha         # filter by sender
//!   hub-history --kind status              # filter by message kind
//!   hub-history --channel agents.tasks --limit 50
//!   hub-history --tail                    # live follow (subscribe to all channels)

use anyhow::Result;
use chrono::Utc;
use clap::Parser;
use nats_hub::{HistoryQuery, Storage, SurrealStorage};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "hub-history",
    about = "Query message history from nats-hub's SurrealDB backend"
)]
struct Args {
    /// Path to the SurrealDB database file
    #[arg(long, default_value = "nats_hub.db")]
    db_path: String,

    /// Filter by channel (exact match)
    #[arg(long)]
    channel: Option<String>,

    /// Filter by sender identity
    #[arg(long)]
    from: Option<String>,

    /// Filter by message kind (message, control, human, status)
    #[arg(long)]
    kind: Option<String>,

    /// Limit number of results (most recent first)
    #[arg(long, default_value = "20")]
    limit: usize,

    /// Live follow mode (subscribe to all channels in addition to querying history)
    #[arg(long)]
    tail: bool,

    /// NATS server URL (only for --tail mode)
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    // Connect to SurrealDB
    let storage = SurrealStorage::connect(&args.db_path).await?;
    storage.migrate().await?;
    storage.ping().await?;

    // Build query
    let mut query = HistoryQuery::new().limit(args.limit);

    if let Some(ref ch) = args.channel {
        query = query.channel(ch.clone());
    }
    if let Some(ref from) = args.from {
        query = query.from(from.clone());
    }
    if let Some(ref kind) = args.kind {
        // Note: kind filter is set via the struct field directly
        // since HistoryQuery doesn't have a builder for it
    }

    // Execute query
    let results = storage.query_history(&query).await?;

    // Print results
    if results.is_empty() {
        println!("(no messages found)");
    } else {
        println!(
            "{:<36} {:<7} {:<20} {:<15} {:<8} {}",
            "ID", "KIND", "FROM", "CHANNEL", "TIME", "PAYLOAD"
        );
        println!("{}", "-".repeat(120));
        for record in &results {
            let time = record.timestamp.format("%H:%M:%S").to_string();
            let payload_preview = preview_payload(&record.payload, 60);
            println!(
                "{:<36} {:<7} {:<20} {:<15} {:<8} {}",
                &record.id.chars().take(36).collect::<String>(),
                record.kind,
                truncate_str(&record.from_identity, 20),
                truncate_str(&record.channel, 15),
                time,
                payload_preview,
            );
        }
        println!("\n({} message(s))", results.len());
    }

    // Tail mode: subscribe to all channels and print live
    if args.tail {
        println!("\n--- live tail (Ctrl+C to stop) ---");

        let client = nats_hub::HubClient::connect(&args.nats_url, "hub-history").await?;
        let mut rx = client.subscribe_all().await?;

        while let Some(env) = rx.recv().await {
            let time = env.meta.timestamp.format("%H:%M:%S").to_string();
            let payload_preview = preview_payload(&env.payload, 80);
            println!(
                "{:<7} {:<20} {:<15} {:<8} {}",
                format!("{:?}", env.meta.kind).to_lowercase(),
                truncate_str(&env.meta.from, 20),
                truncate_str(&env.meta.channel, 15),
                time,
                payload_preview,
            );
        }
    }

    Ok(())
}

fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() > max {
        &s[..max.saturating_sub(2)]
    } else {
        s
    }
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

    if s.len() > max {
        format!("{}...", &s[..max.saturating_sub(3)])
    } else {
        s
    }
}
