//! hub-thread — view conversation threads and pending messages via the query API.
//!
//! Usage:
//!   hub-thread show <root-id>              # full reply chain from a root message
//!   hub-thread show <message-id> --resolve  # follow reply_to to find the root
//!   hub-thread pending --agent worker-1    # unanswered messages for an agent

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use nats_hub::storage::EnvelopeRecord;
use nats_hub::ApiClient;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-thread", about = "View conversation threads and pending messages")]
struct Args {
    #[command(subcommand)]
    command: Command,
    #[arg(long, default_value = "nats://127.0.0.1:4222", global = true)]
    nats_url: String,
}

#[derive(Subcommand)]
enum Command {
    Show { message_id: String, #[arg(long)] resolve: bool, #[arg(long)] json: bool },
    Pending { #[arg(long)] agent: String, #[arg(long)] json: bool },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();
    let args = Args::parse();
    let api = ApiClient::connect(&args.nats_url).await?;

    match args.command {
        Command::Show { message_id, resolve, json } => show_thread(&api, &message_id, resolve, json).await,
        Command::Pending { agent, json } => show_pending(&api, &agent, json).await,
    }
}

async fn show_thread(api: &ApiClient, message_id: &str, resolve: bool, json: bool) -> Result<()> {
    let root_id = if resolve {
        resolve_root_id(api, message_id).await?
    } else {
        message_id.to_string()
    };

    let resp = api.request("thread.get", serde_json::json!({"root_id": &root_id})).await?;
    let mut thread: Vec<EnvelopeRecord> = resp.get("thread").and_then(|t| serde_json::from_value(t.clone()).ok()).unwrap_or_default();

    if thread.is_empty() { bail!("no thread found for message id '{message_id}'"); }
    thread.sort_by_key(|r| r.timestamp);

    if json { println!("{}", serde_json::to_string_pretty(&thread)?); return Ok(()); }

    println!("Thread root: {root_id} ({} message(s))\n", thread.len());
    for record in &thread { print_record(record, &root_id); }
    Ok(())
}

async fn resolve_root_id(api: &ApiClient, start_id: &str) -> Result<String> {
    let mut current = start_id.to_string();
    let mut seen = std::collections::HashSet::new();
    loop {
        if !seen.insert(current.clone()) { bail!("cycle detected in reply_to chain at '{current}'"); }
        let resp = api.request("envelope.get", serde_json::json!({"id": &current})).await?;
        let record: Option<EnvelopeRecord> = resp.get("envelope").and_then(|e| serde_json::from_value(e.clone()).ok());
        let Some(record) = record else { bail!("message '{current}' not found"); };
        match record.reply_to { Some(parent) => current = parent, None => return Ok(current) }
    }
}

async fn show_pending(api: &ApiClient, agent: &str, json: bool) -> Result<()> {
    let resp = api.request("thread.pending", serde_json::json!({"identity": agent})).await?;
    let pending: Vec<EnvelopeRecord> = resp.get("pending").and_then(|p| serde_json::from_value(p.clone()).ok()).unwrap_or_default();

    if json { println!("{}", serde_json::to_string_pretty(&pending)?); return Ok(()); }

    if pending.is_empty() { println!("(no pending messages for {agent})"); return Ok(()); }
    println!("Pending for {agent} ({} message(s))\n", pending.len());
    for record in &pending { print_record(record, &record.id); }
    Ok(())
}

fn print_record(record: &EnvelopeRecord, root_id: &str) {
    let time = record.timestamp.format("%Y-%m-%d %H:%M:%S");
    let is_root = record.id == root_id;
    let prefix = if is_root { "" } else { "  ↳ " };
    let reply_hint = record.reply_to.as_deref().map(|id| format!("  (reply to {id})")).unwrap_or_default();
    println!("{prefix}[{time}] {} @ {} [{}]{}", record.from_identity, record.channel, record.kind, reply_hint, prefix = prefix, time = time);
    println!("{prefix}  id: {}", record.id, prefix = prefix);
    println!("{prefix}  {}", preview_payload(&record.payload), prefix = prefix);
    println!();
}

fn preview_payload(payload: &serde_json::Value) -> String {
    if let Some(text) = payload.get("text").and_then(|v| v.as_str()) { return text.to_string(); }
    if let Some(result) = payload.get("result").and_then(|v| v.as_str()) { return result.to_string(); }
    if let Some(message) = payload.get("message").and_then(|v| v.as_str()) { return message.to_string(); }
    if let Some(prompt) = payload.get("prompt").and_then(|v| v.as_str()) { return prompt.to_string(); }
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
