//! hub-wave — thin CLI over the wave API (docs/WAVES.md).
//!
//! All wave orchestration runs inside hub-server; this binary only calls
//! `hub.api` ops and exits. Watch a running wave with
//! `hub-watch --wave <id>` or poll `hub-wave status <id>`.
//!
//! Usage:
//!   hub-wave create --goal "..." --from orch --tasks tasks.json
//!   hub-wave spawn <wave-id> [--timeout SECS]
//!   hub-wave status <wave-id>
//!   hub-wave cancel <wave-id>
//!   hub-wave list [--status running]

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nats_hub::wave::{validate_tasks, WaveTaskInput};
use nats_hub::{ApiClient, HubClient, MessageKind, WaveRecord, WaveTaskRecord};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-wave", about = "Wave orchestration (server-side)")]
struct Args {
    #[command(subcommand)]
    command: Command,

    #[arg(long, default_value = "nats://127.0.0.1:4222", global = true)]
    nats_url: String,
}

#[derive(Subcommand)]
enum Command {
    /// Create a wave + its tasks (validated server-side, atomically).
    Create {
        goal: String,
        from: String,
        tasks: PathBuf,
    },
    /// Hand a pending wave to the hub-server orchestrator and return.
    Spawn {
        wave_id: String,
        /// Kept for CLI compatibility; orchestration identity is the
        /// hub-server's, not this flag's.
        #[arg(long)]
        from: Option<String>,
        /// Overall wave timeout in seconds (a dead or hung wave fails after it).
        #[arg(long, default_value_t = 3600)]
        timeout: u64,
    },
    /// Wave record, tasks, and merge-gate summary.
    Status { wave_id: String },
    /// Cancel a wave: non-terminal tasks → cancelled, running workers get
    /// the §4.2 cancel DM.
    Cancel { wave_id: String },
    /// List waves (optionally filtered by status).
    List {
        #[arg(long)]
        status: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let args = Args::parse();

    let api = ApiClient::connect(&args.nats_url)
        .await
        .context("connect to NATS for query API")?;

    match args.command {
        Command::Create { goal, from, tasks } => {
            create_wave(&api, &args.nats_url, &goal, &from, &tasks).await
        }
        Command::Spawn {
            wave_id, timeout, ..
        } => spawn_wave(&api, &wave_id, timeout).await,
        Command::Status { wave_id } => show_status(&api, &wave_id).await,
        Command::Cancel { wave_id } => cancel_wave(&api, &wave_id).await,
        Command::List { status } => list_waves(&api, status).await,
    }
}

async fn create_wave(
    api: &ApiClient,
    nats_url: &str,
    goal: &str,
    from: &str,
    tasks_path: &PathBuf,
) -> Result<()> {
    let raw = std::fs::read_to_string(tasks_path)
        .with_context(|| format!("read tasks file {}", tasks_path.display()))?;
    let inputs: Vec<WaveTaskInput> = serde_json::from_str(&raw).context("parse tasks JSON")?;
    // Early local validation for good UX; the API validates again authoritatively.
    validate_tasks(&inputs)?;

    let wave_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let wave = WaveRecord {
        wave_id: wave_id.clone(),
        goal: goal.to_string(),
        status: "pending".to_string(),
        orchestrator: from.to_string(),
        created_at: chrono::Utc::now(),
        closed_at: None,
        metadata: serde_json::json!({}),
    };
    api.request(
        "wave.create",
        serde_json::json!({"wave": wave, "tasks": inputs}),
    )
    .await
    .context("wave.create")?;

    // Publish the manifest on the wave channel (informational).
    let client = HubClient::connect(nats_url, from).await?;
    let wave_channel = nats_hub::subjects::wave_channel_name(&wave_id);
    let manifest = serde_json::json!({
        "action": "wave_manifest", "wave_id": &wave_id, "goal": goal, "tasks": inputs,
    });
    let env = nats_hub::Envelope::new(from, &wave_channel, MessageKind::Control, manifest);
    client.send(&env).await?;
    println!("{wave_id}");
    client.drain().await?;
    Ok(())
}

async fn spawn_wave(api: &ApiClient, wave_id: &str, timeout: u64) -> Result<()> {
    let data = api
        .request(
            "wave.spawn",
            serde_json::json!({"wave_id": wave_id, "timeout_secs": timeout}),
        )
        .await
        .context("wave.spawn")?;
    eprintln!(
        "[hub-wave] wave {wave_id} spawned on the hub-server orchestrator \
         (timeout {timeout}s); watch with: hub-watch --wave {wave_id}"
    );
    print_summary(&data);
    Ok(())
}

async fn show_status(api: &ApiClient, wave_id: &str) -> Result<()> {
    let data = api
        .request("wave.status", serde_json::json!({"wave_id": wave_id}))
        .await
        .context("wave.status")?;
    print_summary(&data);
    let wave: WaveRecord = serde_json::from_value(data["wave"].clone()).context("wave record")?;
    let tasks: Vec<WaveTaskRecord> =
        serde_json::from_value(data["tasks"].clone()).unwrap_or_default();

    let done = tasks.iter().filter(|t| t.status == "done").count();
    println!("wave_id:      {}", wave.wave_id);
    println!("goal:         {}", wave.goal);
    println!("status:       {}", wave.status);
    println!("orchestrator: {}", wave.orchestrator);
    println!("progress:     {done}/{} tasks\n", tasks.len());

    if tasks.is_empty() {
        println!("(no tasks)");
        return Ok(());
    }

    let id_w = tasks
        .iter()
        .map(|t| t.task_id.len())
        .max()
        .unwrap_or(8)
        .max(7);
    let worker_w = tasks
        .iter()
        .map(|t| t.worker.len())
        .max()
        .unwrap_or(6)
        .max(6);
    println!(
        "{:<id_w$}  {:<worker_w$}  {:<9}  {:<6}  DEPS",
        "TASK_ID", "WORKER", "STATUS", "VERIFY"
    );
    println!("{}", "-".repeat(id_w + worker_w + 30));
    for t in &tasks {
        let deps = if t.dependencies.is_empty() {
            "-".to_string()
        } else {
            t.dependencies.join(",")
        };
        let verify = t.verify_result.as_deref().unwrap_or("-");
        println!(
            "{:<id_w$}  {:<worker_w$}  {:<9}  {:<6}  {deps}",
            t.task_id, t.worker, t.status, verify
        );
    }
    Ok(())
}

async fn cancel_wave(api: &ApiClient, wave_id: &str) -> Result<()> {
    let data = api
        .request("wave.cancel", serde_json::json!({"wave_id": wave_id}))
        .await
        .context("wave.cancel")?;
    eprintln!("[hub-wave] cancelled wave {wave_id}");
    print_summary(&data);
    Ok(())
}

fn print_summary(data: &serde_json::Value) {
    let wave_status = data["wave"]["status"].as_str().unwrap_or("?");
    let gate = data["summary"]["merge_gate"].as_str().unwrap_or("?");
    let total = data["summary"]["total"].as_u64().unwrap_or(0);
    let done = data["summary"]["done"].as_u64().unwrap_or(0);
    eprintln!("[hub-wave] status={wave_status} merge_gate={gate} tasks={done}/{total} done");
}

async fn list_waves(api: &ApiClient, status: Option<String>) -> Result<()> {
    let params = match &status {
        Some(s) => serde_json::json!({"status": s}),
        None => serde_json::json!({}),
    };
    let resp = api.request("wave.list", params).await?;
    let waves: Vec<WaveRecord> = resp
        .get("waves")
        .and_then(|w| serde_json::from_value(w.clone()).ok())
        .unwrap_or_default();

    if waves.is_empty() {
        println!("(no waves match)");
        return Ok(());
    }
    let id_w = waves
        .iter()
        .map(|w| w.wave_id.len())
        .max()
        .unwrap_or(8)
        .max(7);
    println!("{:<id_w$}  {:<10}  GOAL", "WAVE_ID", "STATUS");
    println!("{}", "-".repeat(id_w + 20));
    for w in &waves {
        let goal = if w.goal.len() > 60 {
            format!("{}…", &w.goal[..57])
        } else {
            w.goal.clone()
        };
        println!("{:<id_w$}  {:<10}  {goal}", w.wave_id, w.status);
    }
    println!("\n{} wave(s)", waves.len());
    Ok(())
}
