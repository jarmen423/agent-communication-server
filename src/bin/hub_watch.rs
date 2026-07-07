//! hub-watch — watch structured progress events from workers in real time.
//!
//! Usage:
//!   hub-watch --session a3f7b2c1
//!   hub-watch --wave wave-001
//!   hub-watch --agent hermes-worker-1
//!   hub-watch --channel session.a3f7b2c1
//!   hub-watch --all

use anyhow::Result;
use clap::Parser;
use nats_hub::events::{format_event_line, resolve_watch_target, WatchQuery};
use nats_hub::{HubClient, MessageKind};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-watch", about = "Watch structured worker progress events")]
struct Args {
    #[arg(long, conflicts_with_all = ["wave", "agent", "channel", "all"])]
    session: Option<String>,

    #[arg(long, conflicts_with_all = ["session", "agent", "channel", "all"])]
    wave: Option<String>,

    #[arg(long, conflicts_with_all = ["session", "wave", "channel", "all"])]
    agent: Option<String>,

    #[arg(long, conflicts_with_all = ["session", "wave", "agent", "all"])]
    channel: Option<String>,

    #[arg(long, conflicts_with_all = ["session", "wave", "agent", "channel"])]
    all: bool,

    #[arg(long)]
    json: bool,

    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,

    #[arg(long, default_value = "watcher")]
    identity: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let json = args.json;
    let nats_url = args.nats_url.clone();
    let identity = args.identity.clone();
    let query = WatchQuery {
        session: args.session,
        wave: args.wave,
        agent: args.agent,
        channel: args.channel,
        all: args.all,
    };
    let target = resolve_watch_target(&query)?;

    let client = HubClient::connect(&nats_url, &identity).await?;
    let mut rx = client.subscribe_subject(&target.subject).await?;

    eprintln!(
        "[hub-watch] listening on {} ({})…",
        target.label, target.subject
    );
    eprintln!("[hub-watch] press Ctrl+C to stop\n");

    while let Some(env) = rx.recv().await {
        if env.meta.kind != MessageKind::Event {
            continue;
        }
        if let Some(ref agent) = target.agent_filter {
            if env.meta.from != *agent {
                continue;
            }
        }
        if let Some(ref prefix) = target.channel_prefix {
            if !env.meta.channel.starts_with(prefix) {
                continue;
            }
        }

        if json {
            println!("{}", serde_json::to_string(&env)?);
        } else {
            println!("{}", format_event_line(&env));
        }
    }

    eprintln!("[hub-watch] stream ended");
    Ok(())
}
