//! hub-observe — watch messages on channels (for humans or debugging).
//!
//! Usage:
//!   hub-observe                         # watch all channels
//!   hub-observe --channel agents.worker1  # watch a specific channel
//!   hub-observe --json                   # output raw JSON envelopes

use anyhow::Result;
use clap::Parser;
use nats_hub::HubClient;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-observe", about = "Observe messages on nats-hub channels")]
struct Args {
    /// Channel to watch (omitting watches all channels via channel.>)
    #[arg(long)]
    channel: Option<String>,

    /// Output raw JSON envelopes instead of formatted text
    #[arg(long)]
    json: bool,

    /// NATS server URL
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,

    /// Observer identity (for presence tracking)
    #[arg(long, default_value = "observer")]
    identity: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let client = HubClient::connect(&args.nats_url, &args.identity).await?;
    let mut rx = if let Some(ref ch) = args.channel {
        client.subscribe_channel(ch).await?
    } else {
        client.subscribe_all().await?
    };

    eprintln!("[hub-observe] listening on {}…", args.channel.as_deref().unwrap_or("all channels"));
    eprintln!("[hub-observe] press Ctrl+C to stop\n");

    while let Some(env) = rx.recv().await {
        if args.json {
            println!("{}", serde_json::to_string(&env)?);
        } else {
            let ts = env.meta.timestamp.format("%H:%M:%S");
            let kind = match env.meta.kind {
                nats_hub::MessageKind::Message => "MSG",
                nats_hub::MessageKind::Control => "CTL",
                nats_hub::MessageKind::Human => "HUM",
                nats_hub::MessageKind::Status => "STS",
            };
            let dest = env.meta.to.as_deref().unwrap_or("*");
            let payload_str = if let Some(text) = env.payload.get("text").and_then(|v| v.as_str()) {
                text.to_string()
            } else {
                serde_json::to_string_pretty(&env.payload).unwrap_or_default()
            };
            println!("{ts} [{kind}] {} → {dest} @{}", env.meta.from, env.meta.channel);
            println!("  {payload_str}");
            println!();
        }
    }

    eprintln!("[hub-observe] stream ended");
    Ok(())
}