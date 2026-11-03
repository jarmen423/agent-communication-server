//! hub-register — register an agent on the bus with its capabilities.
//!
//! Usage:
//!   hub-register --identity agent-alpha --capabilities compute,observe

use anyhow::Result;
use clap::Parser;
use nats_hub_client::connect_opts::{default_nats_url, sender_identity};
use nats_hub_client::HubClient;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-register", about = "Register an agent on the nats-hub bus")]
struct Args {
    /// Agent identity / name (env: NATS_HUB_IDENTITY)
    #[arg(long)]
    identity: Option<String>,

    /// Comma-separated capabilities (channels the agent is interested in)
    #[arg(long, value_delimiter = ',')]
    capabilities: Vec<String>,

    /// NATS server URL (env: NATS_URL)
    #[arg(long, default_value_t = default_nats_url())]
    nats_url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let identity = sender_identity(args.identity, "--identity")?;
    let client = HubClient::connect(&args.nats_url, &identity).await?;
    client.register(args.capabilities.clone()).await?;
    println!(
        "registered agent '{identity}' with capabilities: {:?}",
        args.capabilities
    );
    Ok(())
}
