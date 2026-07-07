//! hub-agents — list and search persisted agents in the nats-hub storage.
//!
//! Queries the SurrealDB-backed `Storage` directly (does not require NATS
//! to be running). Useful for inspecting the persistent agent registry,
//! auditing capabilities, and finding live agents from the CLI.
//!
//! Usage:
//!   hub-agents                                  # list all known agents
//!   hub-agents --capability compute             # filter by capability
//!   hub-agents --alive 60                       # only agents seen in last 60s
//!   hub-agents --identity agent-alpha           # show one specific agent

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use nats_hub::storage::{AgentFilter, AgentRecord};
#[cfg(feature = "storage-surreal")]
use nats_hub::{Storage, SurrealStorage};

#[derive(Parser)]
#[command(name = "hub-agents", about = "List and search persisted agents")]
struct Args {
    /// Path to the SurrealDB store. Must match the --db-path the hub-server
    /// is using (default: "nats_hub.db").
    #[arg(long, default_value = "nats_hub.db")]
    db_path: String,

    /// Show only agents with ALL of these capabilities (comma-separated).
    #[arg(long, value_delimiter = ',')]
    capability: Vec<String>,

    /// Show only agents seen within the last N seconds.
    #[arg(long)]
    alive: Option<i64>,

    /// Show details for a single agent by identity (overrides other filters).
    #[arg(long)]
    identity: Option<String>,

    /// Limit the number of results.
    #[arg(long)]
    limit: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("nats_hub=info".parse()?))
        .init();

    let args = Args::parse();
    run(args).await
}

async fn run(args: Args) -> Result<()> {
    #[cfg(feature = "storage-surreal")]
    {
        let storage = SurrealStorage::connect(&args.db_path)
            .await
            .with_context(|| format!("failed to open SurrealDB at '{}'", args.db_path))?;
        storage
            .migrate()
            .await
            .context("SurrealDB migration failed")?;
        storage.ping().await.context("SurrealDB ping failed")?;

        if let Some(ident) = args.identity.as_deref() {
            return show_one(&storage, ident).await;
        }

        let mut filter = AgentFilter::new();
        if !args.capability.is_empty() {
            filter = filter.capabilities(args.capability.clone());
        }
        if let Some(secs) = args.alive {
            filter = filter.alive_within(secs);
        }
        if let Some(n) = args.limit {
            filter = filter.limit(n);
        }

        let agents = storage
            .find_agents(&filter)
            .await
            .context("find_agents failed")?;
        print_table(&agents);
        Ok(())
    }

    #[cfg(not(feature = "storage-surreal"))]
    {
        let _ = args;
        anyhow::bail!("hub-agents requires the 'storage-surreal' feature");
    }
}

#[cfg(feature = "storage-surreal")]
async fn show_one(storage: &SurrealStorage, identity: &str) -> Result<()> {
    match storage.get_agent(identity).await? {
        Some(record) => {
            print_table(&[record]);
            Ok(())
        }
        None => {
            eprintln!("no agent with identity '{identity}'");
            std::process::exit(2);
        }
    }
}

fn print_table(agents: &[AgentRecord]) {
    if agents.is_empty() {
        println!("(no agents match)");
        return;
    }

    // Compute column widths from data.
    let id_w = agents
        .iter()
        .map(|a| a.identity.len())
        .max()
        .unwrap_or(8)
        .max("IDENTITY".len());
    let cap_w = agents
        .iter()
        .map(|a| a.capabilities.join(",").len())
        .max()
        .unwrap_or(0)
        .max("CAPABILITIES".len());
    let seen_w = "LAST_SEEN".len();

    println!(
        "{:<id_w$}  {:<cap_w$}  {:<seen_w$}",
        "IDENTITY", "CAPABILITIES", "LAST_SEEN"
    );
    println!("{}", "-".repeat(id_w + cap_w + seen_w + 4));

    let now = chrono::Utc::now();
    for a in agents {
        let ago = format_ago(now, a.last_seen);
        println!(
            "{:<id_w$}  {:<cap_w$}  {ago}",
            a.identity,
            a.capabilities.join(","),
        );
    }

    println!();
    println!("{} agent(s)", agents.len());
}

fn format_ago(now: chrono::DateTime<chrono::Utc>, ts: chrono::DateTime<chrono::Utc>) -> String {
    let delta = now - ts;
    let secs = delta.num_seconds();
    if secs < 0 {
        format!("{}", ts.format("%Y-%m-%d %H:%M:%SZ"))
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}
