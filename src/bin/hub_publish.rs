//! hub-publish — send a message on a channel.
//!
//! Usage:
//!   hub-publish --channel agents.worker1 --from agentA --message "hello"
//!   hub-publish --channel agents.worker1 --from agentA --json '{"task":"compute"}'

use anyhow::Result;
use clap::Parser;
use nats_hub::{Envelope, MessageKind};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-publish", about = "Send a message on a nats-hub channel")]
struct Args {
    /// Channel to publish to
    #[arg(long)]
    channel: String,

    /// Sender identity
    #[arg(long)]
    from: String,

    /// Plain-text message content
    #[arg(long)]
    message: Option<String>,

    /// JSON payload (overrides --message)
    #[arg(long)]
    json: Option<String>,

    /// NATS server URL
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let payload = match args.json {
        Some(j) => serde_json::from_str(&j)?,
        None => serde_json::json!({ "text": args.message.unwrap_or_default() }),
    };

    let client = nats_hub::HubClient::connect(&args.nats_url, &args.from).await?;
    let env = Envelope::new(args.from.clone(), args.channel.clone(), MessageKind::Message, payload);
    let id = env.meta.id.clone();
    client.send(&env).await?;
    println!("sent message id={id} channel={}", args.channel);
    Ok(())
}