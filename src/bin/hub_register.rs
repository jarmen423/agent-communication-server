//! hub-register — register an agent on the bus with its capabilities.
//!
//! Usage:
//!   hub-register --identity agent-alpha --capabilities compute,observe

use anyhow::Result;
use clap::Parser;
use nats_hub::HubClient;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-register", about = "Register an agent on the nats-hub bus")]
struct Args {
    /// Agent identity / name
    #[arg(long)]
    identity: String,

    /// Comma-separated capabilities (channels the agent is interested in)
    #[arg(long, value_delimiter = ',')]
    capabilities: Vec<String>,

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
    let client = HubClient::connect(&args.nats_url, &args.identity).await?;
    client.register(args.capabilities.clone()).await?;
    println!(
        "registered agent '{}' with capabilities: {:?}",
        args.identity, args.capabilities
    );
    Ok(())
}
