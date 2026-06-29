//! hub-server — runs the control plane router.
//!
//! Usage: hub-server [--nats-url URL]
//!
//! Start the NATS server first:  nats-server -c config/nats-server.conf
//! Then:                        cargo run --bin hub-server

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;
use nats_hub::ControlPlane;

#[derive(Parser)]
#[command(name = "hub-server", about = "Run the nats-hub control plane router")]
struct Args {
    /// NATS server URL
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("nats_hub=info".parse()?))
        .init();

    let args = Args::parse();
    let cp = ControlPlane::connect(&args.nats_url).await?;
    info_log("control plane connected, entering routing loop");
    cp.run().await
}

fn info_log(msg: &str) {
    eprintln!("[hub-server] {msg}");
}