//! hub-server — runs the control plane router.
//!
//! Usage: hub-server [--nats-url URL] [--db-path PATH]
//!
//! Start the NATS server first:  nats-server -c config/nats-server.conf
//! Then:                        cargo run --bin hub-server
//!
//! If `--db-path` is provided, the server attaches a SurrealDB-backed
//! `Storage` for persistent agent registry and message history. The DB
//! file is created on first run; subsequent runs warm the in-memory
//! agent cache from the persisted copy.

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use nats_hub::ControlPlane;
#[cfg(feature = "storage-surreal")]
use nats_hub::{Storage, SurrealStorage};

#[derive(Parser)]
#[command(name = "hub-server", about = "Run the nats-hub control plane router")]
struct Args {
    /// NATS server URL
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,

    /// Path to the SurrealDB persistent store. When provided, the server
    /// attaches a Storage backend for the agent registry and message
    /// history. Omit (or pass an empty value) to run in-memory only.
    #[cfg(feature = "storage-surreal")]
    #[arg(long, default_value = "nats_hub.db")]
    db_path: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("nats_hub=info".parse()?),
        )
        .init();

    let args = Args::parse();
    let cp = ControlPlane::connect(&args.nats_url)
        .await
        .context("failed to connect to NATS")?;

    #[cfg(feature = "storage-surreal")]
    {
        if !args.db_path.is_empty() {
            let storage = SurrealStorage::connect(&args.db_path)
                .await
                .with_context(|| {
                    format!("failed to open SurrealDB at '{}'", args.db_path)
                })?;
            storage
                .migrate()
                .await
                .context("SurrealDB migration failed")?;
            storage.ping().await.context("SurrealDB ping failed")?;
            info_log(&format!(
                "attached SurrealDB storage at '{}'",
                args.db_path
            ));
            let cp = cp.with_storage(Arc::new(storage));
            return cp.run().await;
        } else {
            info_log("no --db-path provided, running without persistent storage");
        }
    }

    info_log("control plane connected, entering routing loop");
    cp.run().await
}

fn info_log(msg: &str) {
    eprintln!("[hub-server] {msg}");
}
