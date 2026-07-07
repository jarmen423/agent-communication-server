//! hub-thread — view conversation threads and pending messages from SurrealDB.
//!
//! Usage:
//!   hub-thread show <root-id>              # full reply chain from a root message
//!   hub-thread show <message-id> --resolve  # follow reply_to to find the root
//!   hub-thread pending --agent worker-1    # unanswered messages for an agent
//!   hub-thread show <root-id> --json

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use nats_hub::storage::EnvelopeRecord;
use nats_hub::{Storage, SurrealStorage};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(
    name = "hub-thread",
    about = "View conversation threads and pending messages"
)]
struct Args {
    #[command(subcommand)]
    command: Command,

    #[arg(long, default_value = "nats_hub.db", global = true)]
    db_path: String,
}

#[derive(Subcommand)]
enum Command {
    /// Show a conversation thread starting from a root message ID.
    Show {
        /// Root message ID, or any message ID with --resolve.
        message_id: String,
        /// Walk `reply_to` links to find the thread root before displaying.
        #[arg(long)]
        resolve: bool,
        /// Output raw JSON records.
        #[arg(long)]
        json: bool,
    },
    /// List messages addressed to an agent that have no reply yet.
    Pending {
        /// Agent identity to check.
        #[arg(long)]
        agent: String,
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    run(args).await
}

async fn run(args: Args) -> Result<()> {
    let storage = SurrealStorage::connect(&args.db_path).await?;
    storage.migrate().await?;

    match args.command {
        Command::Show {
            message_id,
            resolve,
            json,
        } => show_thread(&storage, &message_id, resolve, json).await,
        Command::Pending { agent, json } => show_pending(&storage, &agent, json).await,
    }
}

async fn show_thread(
    storage: &SurrealStorage,
    message_id: &str,
    resolve: bool,
    json: bool,
) -> Result<()> {
    let root_id = if resolve {
        resolve_root_id(storage, message_id).await?
    } else {
        message_id.to_string()
    };

    let mut thread = storage.get_thread(&root_id).await?;
    if thread.is_empty() {
        bail!("no thread found for message id '{message_id}'");
    }

    thread.sort_by_key(|r| r.timestamp);

    if json {
        println!("{}", serde_json::to_string_pretty(&thread)?);
        return Ok(());
    }

    println!("Thread root: {root_id} ({} message(s))\n", thread.len());

    for record in &thread {
        print_record(record, &root_id);
    }

    Ok(())
}

async fn resolve_root_id(storage: &SurrealStorage, start_id: &str) -> Result<String> {
    let mut current = start_id.to_string();
    let mut seen = std::collections::HashSet::new();

    loop {
        if !seen.insert(current.clone()) {
            bail!("cycle detected in reply_to chain at '{current}'");
        }

        let Some(record) = storage.get_envelope(&current).await? else {
            bail!("message '{current}' not found");
        };

        match record.reply_to {
            Some(parent) => current = parent,
            None => return Ok(current),
        }
    }
}

async fn show_pending(storage: &SurrealStorage, agent: &str, json: bool) -> Result<()> {
    let pending = storage.list_pending(agent).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&pending)?);
        return Ok(());
    }

    if pending.is_empty() {
        println!("(no pending messages for {agent})");
        return Ok(());
    }

    println!("Pending for {agent} ({} message(s))\n", pending.len());
    for record in &pending {
        print_record(record, &record.id);
    }

    Ok(())
}

fn print_record(record: &EnvelopeRecord, root_id: &str) {
    let time = record.timestamp.format("%Y-%m-%d %H:%M:%S");
    let is_root = record.id == root_id;
    let prefix = if is_root { "" } else { "  ↳ " };
    let reply_hint = record
        .reply_to
        .as_deref()
        .map(|id| format!("  (reply to {id})"))
        .unwrap_or_default();

    println!(
        "{prefix}[{time}] {from} @ {channel} [{kind}]{reply_hint}",
        prefix = prefix,
        time = time,
        from = record.from_identity,
        channel = record.channel,
        kind = record.kind,
        reply_hint = reply_hint,
    );
    println!("{prefix}  id: {}", record.id);
    println!("{prefix}  {}", preview_payload(&record.payload));
    println!();
}

fn preview_payload(payload: &serde_json::Value) -> String {
    if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
        return text.to_string();
    }
    if let Some(result) = payload.get("result").and_then(|v| v.as_str()) {
        return result.to_string();
    }
    if let Some(message) = payload.get("message").and_then(|v| v.as_str()) {
        return message.to_string();
    }
    if let Some(prompt) = payload.get("prompt").and_then(|v| v.as_str()) {
        return prompt.to_string();
    }
    serde_json::to_string(payload).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_payload_prefers_text() {
        let payload = serde_json::json!({"text": "hello", "result": "ignored"});
        assert_eq!(preview_payload(&payload), "hello");
    }
}
