//! hub-interact — interactive REPL for humans to send messages.
//!
//! Usage:
//!   hub-interact --from josh --channel agents.worker1
//!
//! Then type messages and press Enter to send. Type /quit to exit.
//! Type /switch <channel> to change channel mid-session.

use anyhow::Result;
use clap::Parser;
use nats_hub::{Envelope, HubClient, MessageKind};
use std::io::{self, BufRead, Write};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "hub-interact",
    about = "Interactive human messaging on nats-hub"
)]
struct Args {
    /// Sender identity (your name)
    #[arg(long)]
    from: String,

    /// Channel to send to
    #[arg(long, default_value = "agents.broadcast")]
    channel: String,

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
    let client = HubClient::connect(&args.nats_url, &args.from).await?;
    let mut channel = args.channel.clone();

    println!("╔════════════════════════════════════════════╗");
    println!("║         nats-hub interactive mode          ║");
    println!("╚════════════════════════════════════════════╝");
    println!();
    println!("  identity : {}", args.from);
    println!("  channel : {channel}");
    println!("  NATS    : {}", args.nats_url);
    println!();
    println!("  /switch <channel>  — change channel");
    println!("  /status <text>     — send a status update");
    println!("  /quit              — exit");
    println!();

    let stdin = io::stdin();
    loop {
        print!("[{channel}] > ");
        io::stdout().flush()?;

        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        let line = line.trim().to_string();

        if line.is_empty() {
            continue;
        }

        if line.starts_with('/') {
            let parts: Vec<&str> = line.splitn(2, ' ').collect();
            match parts[0] {
                "/quit" | "/exit" | "/q" => {
                    println!("bye!");
                    break;
                }
                "/switch" => {
                    if let Some(ch) = parts.get(1) {
                        channel = ch.to_string();
                        println!("switched to channel: {channel}");
                    } else {
                        eprintln!("usage: /switch <channel>");
                    }
                }
                "/status" => {
                    let text = parts.get(1).copied().unwrap_or("active");
                    let env = Envelope::new(
                        args.from.clone(),
                        channel.clone(),
                        MessageKind::Status,
                        serde_json::json!({ "status": text }),
                    );
                    client.send(&env).await?;
                    println!("status sent: {text}");
                }
                "/help" => {
                    println!("commands: /switch <ch>, /status <text>, /quit");
                }
                _ => {
                    eprintln!("unknown command: {} (try /help)", parts[0]);
                }
            }
        } else {
            let env = Envelope::new(
                args.from.clone(),
                channel.clone(),
                MessageKind::Human,
                serde_json::json!({ "text": line }),
            );
            let id = env.meta.id.clone();
            client.send(&env).await?;
            println!("✓ sent [{id}]");
        }
    }

    Ok(())
}
