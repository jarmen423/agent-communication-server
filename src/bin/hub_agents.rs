//! hub-agents — list and search persisted agents via the query API.
//!
//! Usage:
//!   hub-agents                                  # list all known agents
//!   hub-agents --capability compute             # filter by capability
//!   hub-agents --alive 60                       # only agents seen in last 60s
//!   hub-agents --identity agent-alpha           # show one specific agent

use anyhow::Result;
use clap::Parser;
use nats_hub::storage::{AgentFilter, AgentRecord};
use nats_hub::ApiClient;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-agents", about = "List and search persisted agents")]
struct Args {
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,
    #[arg(long, value_delimiter = ',')]
    capability: Vec<String>,
    #[arg(long)] alive: Option<i64>,
    #[arg(long)] identity: Option<String>,
    #[arg(long)] limit: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("nats_hub=info".parse()?))
        .init();
    let args = Args::parse();
    let api = ApiClient::connect(&args.nats_url).await?;

    if let Some(ident) = &args.identity {
        let resp = api.request("agent.get", serde_json::json!({"identity": ident})).await?;
        let agent: Option<AgentRecord> = resp.get("agent").and_then(|a| serde_json::from_value(a.clone()).ok());
        match agent {
            Some(record) => print_table(&[record]),
            None => { eprintln!("no agent with identity '{ident}'"); std::process::exit(2); }
        }
        return Ok(());
    }

    let mut filter = AgentFilter::new();
    if !args.capability.is_empty() { filter = filter.capabilities(args.capability.clone()); }
    if let Some(secs) = args.alive { filter = filter.alive_within(secs); }
    if let Some(n) = args.limit { filter = filter.limit(n); }

    let resp = api.request("agent.find", serde_json::to_value(&filter)?).await?;
    let agents: Vec<AgentRecord> = resp.get("agents").and_then(|a| serde_json::from_value(a.clone()).ok()).unwrap_or_default();
    print_table(&agents);
    Ok(())
}

fn print_table(agents: &[AgentRecord]) {
    if agents.is_empty() { println!("(no agents match)"); return; }
    let id_w = agents.iter().map(|a| a.identity.len()).max().unwrap_or(8).max("IDENTITY".len());
    let cap_w = agents.iter().map(|a| a.capabilities.join(",").len()).max().unwrap_or(0).max("CAPABILITIES".len());
    println!("{:<id_w$}  {:<cap_w$}  {:<8}", "IDENTITY", "CAPABILITIES", "LAST_SEEN");
    println!("{}", "-".repeat(id_w + cap_w + 12));
    let now = chrono::Utc::now();
    for a in agents {
        let ago = format_ago(now, a.last_seen);
        println!("{:<id_w$}  {:<cap_w$}  {ago}", a.identity, a.capabilities.join(","));
    }
    println!("\n{} agent(s)", agents.len());
}

fn format_ago(now: chrono::DateTime<chrono::Utc>, ts: chrono::DateTime<chrono::Utc>) -> String {
    let secs = (now - ts).num_seconds();
    if secs < 0 { format!("{}", ts.format("%Y-%m-%d %H:%M:%SZ")) }
    else if secs < 60 { format!("{secs}s ago") }
    else if secs < 3600 { format!("{}m ago", secs / 60) }
    else if secs < 86400 { format!("{}h ago", secs / 3600) }
    else { format!("{}d ago", secs / 86400) }
}
