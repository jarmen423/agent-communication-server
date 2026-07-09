//! hub-session — manage stateful agent sessions via the query API.
//!
//! DB operations route through hub-server's query API. NATS messaging
//! (session_start/send/close) still goes directly via HubClient.
//!
//! Usage:
//!   hub-session create --worker hermes-worker-1 --from josh --prompt "Hello"
//!   hub-session send <session-id> --from josh --message "Follow up"
//!   hub-session close <session-id> --from josh
//!   hub-session list [--status active] [--worker hermes-worker-1]
//!   hub-session status <session-id>

use anyhow::Result;
use clap::{Parser, Subcommand};
use nats_hub::storage::{SessionFilter, SessionRecord};
use nats_hub::{ApiClient, HubClient, MessageKind, SessionRecord as SRec};
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-session", about = "Manage stateful agent sessions")]
struct Args {
    #[command(subcommand)]
    command: Command,
    #[arg(long, default_value = "nats://127.0.0.1:4222", global = true)]
    nats_url: String,
}

#[derive(Subcommand)]
enum Command {
    Create {
        #[arg(long)] worker: String,
        #[arg(long)] from: String,
        #[arg(long)] prompt: Option<String>,
        #[arg(long)] model: Option<String>,
        #[arg(long)] provider: Option<String>,
        #[arg(long)] cwd: Option<String>,
        #[arg(long, default_value_t = 30)] timeout: u64,
    },
    Send { session_id: String, #[arg(long)] from: String, #[arg(long)] message: String },
    Close { session_id: String, #[arg(long)] from: String },
    List { #[arg(long)] status: Option<String>, #[arg(long)] worker: Option<String>, #[arg(long)] limit: Option<usize> },
    Status { session_id: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();
    let args = Args::parse();
    let api = ApiClient::connect(&args.nats_url).await?;

    match args.command {
        Command::Create { worker, from, prompt, model, provider, cwd, timeout } => {
            create_session(&api, &args.nats_url, &worker, &from, prompt, model, provider, cwd, timeout).await
        }
        Command::Send { session_id, from, message } => {
            let client = HubClient::connect(&args.nats_url, &from).await?;
            client.send_to_session(&session_id, serde_json::json!({"action": "session_send", "message": message})).await?;
            println!("{session_id}");
            client.drain().await?;
            Ok(())
        }
        Command::Close { session_id, from } => {
            let client = HubClient::connect(&args.nats_url, &from).await?;
            client.send_to_session(&session_id, serde_json::json!({"action": "session_close"})).await?;
            api.request("session.update_status", serde_json::json!({"session_id": &session_id, "status": "closed"})).await?;
            println!("[hub-session] closed {session_id}");
            client.drain().await?;
            Ok(())
        }
        Command::List { status, worker, limit } => list_sessions(&api, status, worker, limit).await,
        Command::Status { session_id } => show_session(&api, &session_id).await,
    }
}

async fn create_session(
    api: &ApiClient, nats_url: &str, worker: &str, from: &str,
    prompt: Option<String>, model: Option<String>, provider: Option<String>,
    cwd: Option<String>, timeout: u64,
) -> Result<()> {
    let client = HubClient::connect(nats_url, from).await?;

    let session_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let session_channel = format!("session.{session_id}");
    let mut session_rx = client.subscribe_session(&session_id).await?;

    let mut start_payload = serde_json::json!({});
    if let Some(p) = &prompt { start_payload["prompt"] = serde_json::json!(p); }
    if let Some(m) = &model { start_payload["model"] = serde_json::json!(m); }
    if let Some(p) = &provider { start_payload["provider"] = serde_json::json!(p); }
    if let Some(c) = &cwd { start_payload["cwd"] = serde_json::json!(c); }

    if let serde_json::Value::Object(ref mut map) = start_payload {
        map.insert("action".into(), serde_json::json!("session_start"));
        map.insert("session_id".into(), serde_json::json!(&session_id));
        map.insert("session_channel".into(), serde_json::json!(&session_channel));
    }

    let env = nats_hub::Envelope::new(from.to_string(), &session_channel, MessageKind::Message, start_payload.clone()).to(worker);
    client.send(&env).await?;

    // Persist session via query API
    let now = chrono::Utc::now();
    let record = SessionRecord {
        session_id: session_id.clone(), orchestrator: from.to_string(), worker: worker.to_string(),
        status: "active".to_string(), cwd: cwd.clone(), model: model.clone(), provider: provider.clone(),
        created_at: now, updated_at: now, closed_at: None, metadata: serde_json::json!({}),
    };
    let _ = api.request("session.create", serde_json::to_value(&record)?).await;

    let ready = match tokio::time::timeout(Duration::from_secs(timeout), session_rx.recv()).await {
        Ok(Some(env)) => env.meta.kind == MessageKind::Status && env.payload.get("status").and_then(|v| v.as_str()) == Some("ready"),
        Ok(None) => false,
        Err(_) => { eprintln!("[hub-session] TIMEOUT: no ready signal from {worker} after {timeout}s"); std::process::exit(2); }
    };
    if ready { eprintln!("[hub-session] worker {worker} is ready"); }
    else { eprintln!("[hub-session] warning: no ready signal received (session still created)"); }
    println!("{session_id}");
    client.drain().await?;
    Ok(())
}

async fn list_sessions(api: &ApiClient, status: Option<String>, worker: Option<String>, limit: Option<usize>) -> Result<()> {
    let mut filter = SessionFilter::new();
    if let Some(s) = status { filter = filter.status(s); }
    if let Some(w) = worker { filter = filter.worker(w); }
    if let Some(n) = limit { filter = filter.limit(n); }

    let resp = api.request("session.list", serde_json::to_value(&filter)?).await?;
    let sessions: Vec<SessionRecord> = resp.get("sessions").and_then(|s| serde_json::from_value(s.clone()).ok()).unwrap_or_default();
    print_session_table(&sessions);
    Ok(())
}

async fn show_session(api: &ApiClient, session_id: &str) -> Result<()> {
    let resp = api.request("session.get", serde_json::json!({"session_id": session_id})).await?;
    let session: Option<SessionRecord> = resp.get("session").and_then(|s| serde_json::from_value(s.clone()).ok());
    match session {
        Some(s) => {
            println!("session_id:  {}", s.session_id);
            println!("orchestrator: {}", s.orchestrator);
            println!("worker:       {}", s.worker);
            println!("status:       {}", s.status);
            if let Some(cwd) = s.cwd { println!("cwd:          {cwd}"); }
            if let Some(model) = s.model { println!("model:        {model}"); }
            if let Some(provider) = s.provider { println!("provider:     {provider}"); }
            println!("created_at:   {}", s.created_at.format("%Y-%m-%d %H:%M:%SZ"));
            if let Some(closed) = s.closed_at { println!("closed_at:    {}", closed.format("%Y-%m-%d %H:%M:%SZ")); }
            Ok(())
        }
        None => { eprintln!("no session with id '{session_id}'"); std::process::exit(2); }
    }
}

fn print_session_table(sessions: &[SessionRecord]) {
    if sessions.is_empty() { println!("(no sessions match)"); return; }
    let id_w = sessions.iter().map(|s| s.session_id.len()).max().unwrap_or(10).max("SESSION_ID".len());
    let worker_w = sessions.iter().map(|s| s.worker.len()).max().unwrap_or(6).max("WORKER".len());
    let status_w = "STATUS".len();
    println!("{:<id_w$}  {:<worker_w$}  {:<status_w$}  CREATED", "SESSION_ID", "WORKER", "STATUS");
    println!("{}", "-".repeat(id_w + worker_w + status_w + 16));
    for s in sessions {
        let created = s.created_at.format("%Y-%m-%d %H:%M");
        println!("{:<id_w$}  {:<worker_w$}  {:<status_w$}  {created}", s.session_id, s.worker, s.status);
    }
    println!("\n{} session(s)", sessions.len());
}
