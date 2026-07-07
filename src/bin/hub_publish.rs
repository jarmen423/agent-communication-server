//! hub-publish — send a message on a channel.
//!
//! Usage:
//!   hub-publish --channel agents.worker1 --from agentA --message "hello"
//!   hub-publish --channel agents.worker1 --from agentA --json '{"task":"compute"}'
//!   hub-publish --channel agents.tasks --to worker-1 --from agentA --message "do X"   # DM
//!   hub-publish --channel task.abc --from agentA --kind event --json '{"event_type":"progress"}'

use anyhow::{Context, Result};
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

    /// Direct recipient — routes to channel.inbox.<to> instead of broadcasting
    #[arg(long)]
    to: Option<String>,

    /// Message kind: message (default), control, human, status, event
    #[arg(long, default_value = "message")]
    kind: String,

    /// NATS server URL
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
}

fn parse_kind(s: &str) -> Result<MessageKind> {
    match s.to_ascii_lowercase().as_str() {
        "message" | "msg" => Ok(MessageKind::Message),
        "control" => Ok(MessageKind::Control),
        "human" => Ok(MessageKind::Human),
        "status" => Ok(MessageKind::Status),
        "event" => Ok(MessageKind::Event),
        other => Err(anyhow::anyhow!(
            "unknown --kind '{}' (expected message|control|human|status|event)",
            other
        )),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();

    let payload = match args.json {
        Some(j) => serde_json::from_str(&j).context("invalid --json payload")?,
        None => serde_json::json!({ "text": args.message.unwrap_or_default() }),
    };

    let kind = parse_kind(&args.kind)?;

    let client = nats_hub::HubClient::connect(&args.nats_url, &args.from).await?;
    let mut env = Envelope::new(args.from.clone(), args.channel.clone(), kind, payload);
    if let Some(recipient) = &args.to {
        env = env.to(recipient.clone());
    }
    let id = env.meta.id.clone();
    client.send(&env).await?;
    match &args.to {
        Some(to) => println!("sent DM id={id} channel={} to={to}", args.channel),
        None => println!("sent message id={id} channel={}", args.channel),
    }
    Ok(())
}
