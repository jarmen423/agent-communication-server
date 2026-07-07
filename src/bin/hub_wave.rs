//! hub-wave — orchestrate parallel agent tasks with disjoint write scopes.
//!
//! Usage:
//!   hub-wave create --goal "..." --from orch --tasks tasks.json
//!   hub-wave spawn <wave-id> --from orch
//!   hub-wave status <wave-id>
//!   hub-wave close <wave-id> --from orch
//!   hub-wave list [--status running]

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use nats_hub::protocol::subjects;
use nats_hub::wave::{evaluate_merge_gate, spawn_wave, validate_tasks, WaveTaskInput};
use nats_hub::{HubClient, MessageKind, Storage, SurrealStorage, WaveRecord, WaveTaskRecord};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-wave", about = "Orchestrate parallel wave tasks")]
struct Args {
    #[command(subcommand)]
    command: Command,

    #[arg(long, default_value = "nats://127.0.0.1:4222", global = true)]
    nats_url: String,

    #[arg(long, default_value = "nats_hub.db", global = true)]
    db_path: String,
}

#[derive(Subcommand)]
enum Command {
    /// Create a wave from a tasks JSON file.
    Create {
        #[arg(long)]
        goal: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        tasks: PathBuf,
    },
    /// Spawn tasks (respecting dependencies) and wait for completion.
    Spawn {
        wave_id: String,
        #[arg(long)]
        from: String,
        #[arg(long, default_value_t = 3600)]
        timeout: u64,
    },
    /// Show wave + task statuses.
    Status { wave_id: String },
    /// Close a wave after evaluating the merge gate.
    Close {
        wave_id: String,
        #[arg(long)]
        from: String,
    },
    /// List waves from the DB.
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
    run(args).await
}

async fn run(args: Args) -> Result<()> {
    match args.command {
        Command::Create { goal, from, tasks } => {
            create_wave(&args.nats_url, &args.db_path, &goal, &from, &tasks).await
        }
        Command::Spawn {
            wave_id,
            from,
            timeout,
        } => spawn_command(&args.nats_url, &args.db_path, &wave_id, &from, timeout).await,
        Command::Status { wave_id } => show_status(&args.db_path, &wave_id).await,
        Command::Close { wave_id, from } => {
            close_wave(&args.nats_url, &args.db_path, &wave_id, &from).await
        }
        Command::List { status } => list_waves(&args.db_path, status).await,
    }
}

async fn create_wave(
    nats_url: &str,
    db_path: &str,
    goal: &str,
    from: &str,
    tasks_path: &PathBuf,
) -> Result<()> {
    let raw = std::fs::read_to_string(tasks_path)
        .with_context(|| format!("read tasks file {}", tasks_path.display()))?;
    let inputs: Vec<WaveTaskInput> = serde_json::from_str(&raw).context("parse tasks JSON")?;
    validate_tasks(&inputs)?;

    let wave_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let now = chrono::Utc::now();

    let storage = SurrealStorage::connect(db_path).await?;
    storage.migrate().await?;

    let wave = WaveRecord {
        wave_id: wave_id.clone(),
        goal: goal.to_string(),
        status: "pending".to_string(),
        orchestrator: from.to_string(),
        created_at: now,
        closed_at: None,
        metadata: serde_json::json!({}),
    };
    storage.create_wave(wave).await?;

    for input in &inputs {
        let task = WaveTaskRecord {
            wave_id: wave_id.clone(),
            task_id: input.task_id.clone(),
            worker: input.worker.clone(),
            goal: input.goal.clone(),
            status: "pending".to_string(),
            write_scope: input.write_scope.clone(),
            dependencies: input.dependencies.clone(),
            handoff_path: input.handoff_path.clone(),
            verify_cmd: input.verify_cmd.clone(),
            created_at: now,
            started_at: None,
            completed_at: None,
            result: None,
        };
        storage.create_wave_task(task).await?;
    }

    let client = HubClient::connect(nats_url, from).await?;
    let wave_channel = subjects::wave_channel_name(&wave_id);
    let manifest = serde_json::json!({
        "action": "wave_manifest",
        "wave_id": wave_id,
        "goal": goal,
        "tasks": inputs,
    });
    let env = nats_hub::Envelope::new(from, &wave_channel, MessageKind::Control, manifest);
    client.send(&env).await?;

    println!("{wave_id}");
    client.drain().await?;
    Ok(())
}

async fn spawn_command(
    nats_url: &str,
    db_path: &str,
    wave_id: &str,
    from: &str,
    timeout: u64,
) -> Result<()> {
    let storage = SurrealStorage::connect(db_path).await?;
    storage.migrate().await?;

    let client = HubClient::connect(nats_url, from).await?;
    let outcome = spawn_wave(&client, &storage, wave_id, timeout).await?;
    client.drain().await?;

    match outcome {
        nats_hub::SpawnOutcome::Completed => {
            eprintln!("[hub-wave] wave {wave_id} completed");
            Ok(())
        }
        nats_hub::SpawnOutcome::Failed => {
            eprintln!("[hub-wave] wave {wave_id} failed");
            std::process::exit(1);
        }
        nats_hub::SpawnOutcome::Timeout => {
            eprintln!("[hub-wave] wave {wave_id} timed out after {timeout}s");
            std::process::exit(2);
        }
    }
}

async fn show_status(db_path: &str, wave_id: &str) -> Result<()> {
    let storage = SurrealStorage::connect(db_path).await?;
    storage.migrate().await?;

    let wave = storage
        .get_wave(wave_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("wave '{wave_id}' not found"))?;
    let tasks = storage.list_wave_tasks(wave_id).await?;

    let done = tasks.iter().filter(|t| t.status == "done").count();
    let pct = if tasks.is_empty() {
        0
    } else {
        (done * 100) / tasks.len()
    };

    println!("wave_id:      {}", wave.wave_id);
    println!("goal:         {}", wave.goal);
    println!("status:       {}", wave.status);
    println!("orchestrator: {}", wave.orchestrator);
    println!("progress:     {done}/{} tasks ({pct}%)", tasks.len());
    println!();

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
        "{:<id_w$}  {:<worker_w$}  {:<8}  DEPS",
        "TASK_ID", "WORKER", "STATUS"
    );
    println!("{}", "-".repeat(id_w + worker_w + 20));
    for t in &tasks {
        let deps = if t.dependencies.is_empty() {
            "-".to_string()
        } else {
            t.dependencies.join(",")
        };
        println!(
            "{:<id_w$}  {:<worker_w$}  {:<8}  {deps}",
            t.task_id, t.worker, t.status
        );
    }
    Ok(())
}

async fn close_wave(nats_url: &str, db_path: &str, wave_id: &str, from: &str) -> Result<()> {
    let storage = SurrealStorage::connect(db_path).await?;
    storage.migrate().await?;

    let tasks = storage.list_wave_tasks(wave_id).await?;
    let final_status = evaluate_merge_gate(&tasks);
    if final_status == "running" {
        bail!("wave '{wave_id}' is still running — not all tasks are done");
    }

    storage.update_wave_status(wave_id, final_status).await?;

    let client = HubClient::connect(nats_url, from).await?;
    let wave_channel = subjects::wave_channel_name(wave_id);
    let payload = serde_json::json!({
        "action": "wave_close",
        "wave_id": wave_id,
        "status": final_status,
    });
    client.send_message(&wave_channel, payload).await?;
    client.drain().await?;

    eprintln!("[hub-wave] closed {wave_id} as {final_status}");
    Ok(())
}

async fn list_waves(db_path: &str, status: Option<String>) -> Result<()> {
    let storage = SurrealStorage::connect(db_path).await?;
    storage.migrate().await?;

    let waves = storage.list_waves(status.as_deref()).await?;

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
    println!();
    println!("{} wave(s)", waves.len());
    Ok(())
}
