//! hub-session — manage stateful agent sessions.
//!
//! Create, send messages to, close, and list persistent sessions.
//! Sessions are multi-turn conversations on `channel.session.<uuid>`.
//!
//! Usage:
//!   hub-session create --worker hermes-worker-1 --from josh --prompt "Hello"
//!   hub-session send <session-id> --from josh --message "Follow up"
//!   hub-session close <session-id> --from josh
//!   hub-session list [--status active] [--worker hermes-worker-1]
//!   hub-session status <session-id>

use anyhow::Result;
use clap::{Parser, Subcommand};
use nats_hub::{HubClient, MessageKind};
use std::time::Duration;
use tracing_subscriber::EnvFilter;

#[cfg(feature = "storage-surreal")]
use nats_hub::storage::{SessionFilter, SessionRecord};
#[cfg(feature = "storage-surreal")]
use nats_hub::{Storage, SurrealStorage};

#[derive(Parser)]
#[command(name = "hub-session", about = "Manage stateful agent sessions")]
struct Args {
    #[command(subcommand)]
    command: Command,

    /// NATS server URL.
    #[arg(long, default_value = "nats://127.0.0.1:4222", global = true)]
    nats_url: String,

    /// Path to the SurrealDB store (must match hub-server --db-path).
    #[arg(long, default_value = "nats_hub.db", global = true)]
    db_path: String,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new session with a worker.
    Create {
        /// Worker agent identity to start the session with.
        #[arg(long)]
        worker: String,
        /// Your identity (the orchestrator).
        #[arg(long)]
        from: String,
        /// Initial prompt to send with the session start.
        #[arg(long)]
        prompt: Option<String>,
        /// Model hint for the worker.
        #[arg(long)]
        model: Option<String>,
        /// Provider hint for the worker.
        #[arg(long)]
        provider: Option<String>,
        /// Working directory hint.
        #[arg(long)]
        cwd: Option<String>,
        /// Timeout in seconds to wait for worker's "ready" status.
        #[arg(long, default_value_t = 30)]
        timeout: u64,
    },
    /// Send a follow-up message on an existing session.
    Send {
        /// Session ID (from `create`).
        session_id: String,
        /// Your identity.
        #[arg(long)]
        from: String,
        /// Message to send.
        #[arg(long)]
        message: String,
    },
    /// Close a session.
    Close {
        /// Session ID to close.
        session_id: String,
        /// Your identity.
        #[arg(long)]
        from: String,
    },
    /// List sessions (queries DB, no NATS needed).
    List {
        /// Filter by status (active, closed).
        #[arg(long)]
        status: Option<String>,
        /// Filter by worker identity.
        #[arg(long)]
        worker: Option<String>,
        /// Limit number of results.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show details for a single session.
    Status {
        /// Session ID to show.
        session_id: String,
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
    match args.command {
        Command::Create {
            worker,
            from,
            prompt,
            model,
            provider,
            cwd,
            timeout,
        } => {
            create_session(
                &args.nats_url,
                &args.db_path,
                &worker,
                &from,
                prompt,
                model,
                provider,
                cwd,
                timeout,
            )
            .await
        }
        Command::Send {
            session_id,
            from,
            message,
        } => {
            let client = HubClient::connect(&args.nats_url, &from).await?;
            client
                .send_to_session(&session_id, serde_json::json!({"message": message}))
                .await?;
            println!("{session_id}");
            client.drain().await?;
            Ok(())
        }
        Command::Close { session_id, from } => {
            let client = HubClient::connect(&args.nats_url, &from).await?;
            client.close_session(&session_id).await?;
            // Update DB status
            #[cfg(feature = "storage-surreal")]
            {
                let storage = SurrealStorage::connect(&args.db_path).await?;
                storage.update_session_status(&session_id, "closed").await?;
            }
            println!("[hub-session] closed {session_id}");
            client.drain().await?;
            Ok(())
        }
        Command::List {
            status,
            worker,
            limit,
        } => list_sessions(&args.db_path, status, worker, limit).await,
        Command::Status { session_id } => show_session(&args.db_path, &session_id).await,
    }
}

async fn create_session(
    nats_url: &str,
    db_path: &str,
    worker: &str,
    from: &str,
    prompt: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    cwd: Option<String>,
    timeout: u64,
) -> Result<()> {
    let client = HubClient::connect(nats_url, from).await?;

    // Build payload for session_start
    let mut payload = serde_json::json!({});
    if let Some(p) = &prompt {
        payload["prompt"] = serde_json::json!(p);
    }
    if let Some(m) = &model {
        payload["model"] = serde_json::json!(m);
    }
    if let Some(p) = &provider {
        payload["provider"] = serde_json::json!(p);
    }
    if let Some(c) = &cwd {
        payload["cwd"] = serde_json::json!(c);
    }

    // Subscribe to session channel BEFORE sending session_start
    // (we need the session_id first, so we do a two-step: generate ID,
    // subscribe, then send)
    let session_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let session_channel = format!("session.{session_id}");

    // Subscribe to the session channel to listen for ready status
    let mut session_rx = client.subscribe_session(&session_id).await?;

    // Build and send the session_start envelope (DM to worker)
    let mut start_payload = payload;
    if let serde_json::Value::Object(ref mut map) = start_payload {
        map.insert("action".into(), serde_json::json!("session_start"));
        map.insert("session_id".into(), serde_json::json!(&session_id));
        map.insert(
            "session_channel".into(),
            serde_json::json!(&session_channel),
        );
    }

    let env = nats_hub::Envelope::new(
        from.to_string(),
        &session_channel,
        MessageKind::Message,
        start_payload.clone(),
    )
    .to(worker);
    client.send(&env).await?;

    // Persist session to DB
    #[cfg(feature = "storage-surreal")]
    {
        let storage = SurrealStorage::connect(db_path).await?;
        storage.migrate().await?;
        let now = chrono::Utc::now();
        let record = SessionRecord {
            session_id: session_id.clone(),
            orchestrator: from.to_string(),
            worker: worker.to_string(),
            status: "active".to_string(),
            cwd: cwd.clone(),
            model: model.clone(),
            provider: provider.clone(),
            created_at: now,
            updated_at: now,
            closed_at: None,
            metadata: serde_json::json!({}),
        };
        storage.create_session(record).await?;
    }

    // Wait for worker's "ready" status on the session channel
    let timeout_dur = Duration::from_secs(timeout);
    let ready = match tokio::time::timeout(timeout_dur, session_rx.recv()).await {
        Ok(Some(env)) => {
            env.meta.kind == MessageKind::Status
                && env.payload.get("status").and_then(|v| v.as_str()) == Some("ready")
        }
        Ok(None) => false,
        Err(_) => {
            eprintln!("[hub-session] TIMEOUT: no ready signal from {worker} after {timeout}s");
            std::process::exit(2);
        }
    };

    if ready {
        eprintln!("[hub-session] worker {worker} is ready");
    } else {
        eprintln!("[hub-session] warning: no ready signal received (session still created)");
    }

    println!("{session_id}");
    client.drain().await?;
    Ok(())
}

#[cfg(feature = "storage-surreal")]
async fn list_sessions(
    db_path: &str,
    status: Option<String>,
    worker: Option<String>,
    limit: Option<usize>,
) -> Result<()> {
    let storage = SurrealStorage::connect(db_path).await?;
    storage.migrate().await?;

    let mut filter = SessionFilter::new();
    if let Some(s) = status {
        filter = filter.status(s);
    }
    if let Some(w) = worker {
        filter = filter.worker(w);
    }
    if let Some(n) = limit {
        filter = filter.limit(n);
    }

    let sessions = storage.list_sessions(&filter).await?;
    print_session_table(&sessions);
    Ok(())
}

#[cfg(not(feature = "storage-surreal"))]
async fn list_sessions(
    _db_path: &str,
    _status: Option<String>,
    _worker: Option<String>,
    _limit: Option<usize>,
) -> Result<()> {
    anyhow::bail!("hub-session list requires the 'storage-surreal' feature");
}

#[cfg(feature = "storage-surreal")]
async fn show_session(db_path: &str, session_id: &str) -> Result<()> {
    let storage = SurrealStorage::connect(db_path).await?;
    storage.migrate().await?;

    match storage.get_session(session_id).await? {
        Some(s) => {
            println!("session_id:  {}", s.session_id);
            println!("orchestrator: {}", s.orchestrator);
            println!("worker:       {}", s.worker);
            println!("status:       {}", s.status);
            if let Some(cwd) = s.cwd {
                println!("cwd:          {cwd}");
            }
            if let Some(model) = s.model {
                println!("model:        {model}");
            }
            if let Some(provider) = s.provider {
                println!("provider:     {provider}");
            }
            println!("created_at:   {}", s.created_at.format("%Y-%m-%d %H:%M:%SZ"));
            if let Some(closed) = s.closed_at {
                println!("closed_at:    {}", closed.format("%Y-%m-%d %H:%M:%SZ"));
            }
            Ok(())
        }
        None => {
            eprintln!("no session with id '{session_id}'");
            std::process::exit(2);
        }
    }
}

#[cfg(not(feature = "storage-surreal"))]
async fn show_session(_db_path: &str, _session_id: &str) -> Result<()> {
    anyhow::bail!("hub-session status requires the 'storage-surreal' feature");
}

#[cfg(feature = "storage-surreal")]
fn print_session_table(sessions: &[SessionRecord]) {
    if sessions.is_empty() {
        println!("(no sessions match)");
        return;
    }

    let id_w = sessions
        .iter()
        .map(|s| s.session_id.len())
        .max()
        .unwrap_or(10)
        .max("SESSION_ID".len());
    let worker_w = sessions
        .iter()
        .map(|s| s.worker.len())
        .max()
        .unwrap_or(6)
        .max("WORKER".len());
    let status_w = "STATUS".len();

    println!(
        "{:<id_w$}  {:<worker_w$}  {:<status_w$}  CREATED",
        "SESSION_ID", "WORKER", "STATUS"
    );
    println!(
        "{}",
        "-".repeat(id_w + worker_w + status_w + 16)
    );

    for s in sessions {
        let created = s.created_at.format("%Y-%m-%d %H:%M");
        println!(
            "{:<id_w$}  {:<worker_w$}  {:<status_w$}  {created}",
            s.session_id, s.worker, s.status,
        );
    }

    println!();
    println!("{} session(s)", sessions.len());
}
