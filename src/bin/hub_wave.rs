//! hub-wave — orchestrate parallel agent tasks with disjoint write scopes.
//!
//! Routes DB operations through hub-server's query API (NATS request-reply)
//! to avoid RocksDB single-writer lock contention.
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
use nats_hub::wave::{evaluate_merge_gate, validate_tasks, WaveTaskInput};
use nats_hub::{ApiClient, HubClient, MessageKind, WaveRecord, WaveTaskRecord};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "hub-wave", about = "Orchestrate parallel wave tasks")]
struct Args {
    #[command(subcommand)]
    command: Command,

    #[arg(long, default_value = "nats://127.0.0.1:4222", global = true)]
    nats_url: String,

    /// DB path (only used when hub-server is NOT running).
    /// When hub-server is running, operations route via query API.
    #[arg(long, default_value = "nats_hub.db", global = true)]
    db_path: String,
}

#[derive(Subcommand)]
enum Command {
    Create { goal: String, from: String, tasks: PathBuf },
    Spawn { wave_id: String, from: String, #[arg(long, default_value_t = 3600)] timeout: u64 },
    Status { wave_id: String },
    Close { wave_id: String, from: String },
    List { #[arg(long)] status: Option<String> },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    let args = Args::parse();

    let api = ApiClient::connect(&args.nats_url).await.context("connect to NATS for query API")?;

    match args.command {
        Command::Create { goal, from, tasks } => {
            create_wave(&api, &args.nats_url, &goal, &from, &tasks).await
        }
        Command::Spawn { wave_id, from, timeout } => {
            spawn_command(&api, &args.nats_url, &wave_id, &from, timeout).await
        }
        Command::Status { wave_id } => show_status(&api, &wave_id).await,
        Command::Close { wave_id, from } => {
            close_wave(&api, &args.nats_url, &wave_id, &from).await
        }
        Command::List { status } => list_waves(&api, status).await,
    }
}

async fn create_wave(
    api: &ApiClient, nats_url: &str, goal: &str, from: &str, tasks_path: &PathBuf,
) -> Result<()> {
    let raw = std::fs::read_to_string(tasks_path)
        .with_context(|| format!("read tasks file {}", tasks_path.display()))?;
    let inputs: Vec<WaveTaskInput> = serde_json::from_str(&raw).context("parse tasks JSON")?;
    validate_tasks(&inputs)?;

    let wave_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
    let now = chrono::Utc::now();

    let wave = WaveRecord {
        wave_id: wave_id.clone(), goal: goal.to_string(), status: "pending".to_string(),
        orchestrator: from.to_string(), created_at: now, closed_at: None,
        metadata: serde_json::json!({}),
    };
    api.request("wave.create", serde_json::to_value(&wave)?).await?;

    for input in &inputs {
        let task = WaveTaskRecord {
            wave_id: wave_id.clone(), task_id: input.task_id.clone(),
            worker: input.worker.clone(), goal: input.goal.clone(),
            status: "pending".to_string(), write_scope: input.write_scope.clone(),
            dependencies: input.dependencies.clone(), handoff_path: input.handoff_path.clone(),
            verify_cmd: input.verify_cmd.clone(), created_at: now, started_at: None,
            completed_at: None, result: None,
        };
        api.request("wave.create_task", serde_json::to_value(&task)?).await?;
    }

    // Publish manifest on the wave channel
    let client = HubClient::connect(nats_url, from).await?;
    let wave_channel = subjects::wave_channel_name(&wave_id);
    let manifest = serde_json::json!({
        "action": "wave_manifest", "wave_id": &wave_id, "goal": goal, "tasks": inputs,
    });
    let env = nats_hub::Envelope::new(from, &wave_channel, MessageKind::Control, manifest);
    client.send(&env).await?;
    println!("{wave_id}");
    client.drain().await?;
    Ok(())
}

async fn spawn_command(
    api: &ApiClient, nats_url: &str, wave_id: &str, from: &str, timeout: u64,
) -> Result<()> {
    // Fetch tasks via query API
    let resp = api.request("wave.list_tasks", serde_json::json!({"wave_id": wave_id})).await?;
    let tasks: Vec<WaveTaskRecord> = resp.get("tasks")
        .and_then(|t| serde_json::from_value(t.clone()).ok())
        .unwrap_or_default();

    if tasks.is_empty() { bail!("no tasks found for wave '{wave_id}'"); }

    api.request("wave.update_status", serde_json::json!({"wave_id": wave_id, "status": "running"})).await?;

    // Connect HubClient for NATS message dispatch + event subscription
    let client = HubClient::connect(nats_url, from).await?;

    // Use the existing spawn_wave logic — it subscribes to events and dispatches tasks.
    // But spawn_wave takes a Storage trait. We need an adapter.
    // For now, inline the spawn logic using the ApiClient for DB ops.

    let outcome = spawn_via_api(&client, api, wave_id, &tasks, timeout).await?;
    client.drain().await?;

    match outcome {
        SpawnOutcome::Completed => { eprintln!("[hub-wave] wave {wave_id} completed"); Ok(()) }
        SpawnOutcome::Failed => { eprintln!("[hub-wave] wave {wave_id} failed"); std::process::exit(1) }
        SpawnOutcome::Timeout => { eprintln!("[hub-wave] wave {wave_id} timed out"); std::process::exit(2) }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SpawnOutcome { Completed, Failed, Timeout }

async fn spawn_via_api(
    client: &HubClient, api: &ApiClient, wave_id: &str,
    tasks: &[WaveTaskRecord], timeout_secs: u64,
) -> Result<SpawnOutcome> {
    use std::collections::{HashMap, HashSet};
    use std::time::Duration;
    use nats_hub::events::{event_type, event_types};

    let mut task_map: HashMap<String, WaveTaskRecord> =
        tasks.iter().map(|t| (t.task_id.clone(), t.clone())).collect();
    let mut completed: HashSet<String> = HashSet::new();
    let mut failed = false;

    let wave_prefix = subjects::wave_channel_name(wave_id);
    let mut event_rx = client.subscribe_subject(&format!("channel.{wave_prefix}.>")).await?;
    let mut wave_rx = client.subscribe_channel(&wave_prefix).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        if failed {
            api.request("wave.update_status", serde_json::json!({"wave_id": wave_id, "status": "failed"})).await.ok();
            return Ok(SpawnOutcome::Failed);
        }

        let all_terminal = task_map.values().all(|t| t.status == "done" || t.status == "failed");
        if all_terminal {
            let any_failed = task_map.values().any(|t| t.status == "failed");
            let final_status = if any_failed { "failed" } else { "completed" };
            api.request("wave.update_status", serde_json::json!({"wave_id": wave_id, "status": final_status})).await.ok();
            return Ok(if any_failed { SpawnOutcome::Failed } else { SpawnOutcome::Completed });
        }

        // Start ready tasks — collect task_ids first to avoid borrow conflicts
        let ready_ids: Vec<String> = task_map.values()
            .filter(|t| t.status == "pending")
            .filter(|t| t.dependencies.iter().all(|d| completed.contains(d)))
            .map(|t| t.task_id.clone())
            .collect();

        for task_id in &ready_ids {
            let task = match task_map.get(task_id) {
                Some(t) => t.clone(),
                None => continue,
            };
            start_task(client, wave_id, &task).await?;
            api.request("wave.update_task_status", serde_json::json!({
                "wave_id": wave_id, "task_id": &task.task_id, "status": "running"
            })).await?;
            if let Some(e) = task_map.get_mut(&task.task_id) { e.status = "running".into(); }
            tracing::info!(wave_id, task_id = %task.task_id, "started task");
        }

        let remaining = Duration::from_secs(1).min(
            deadline.checked_duration_since(tokio::time::Instant::now()).unwrap_or_default()
        );
        if remaining.is_zero() {
            api.request("wave.update_status", serde_json::json!({"wave_id": wave_id, "status": "failed"})).await.ok();
            return Ok(SpawnOutcome::Timeout);
        }

        let env = tokio::select! {
            m = event_rx.recv() => m,
            m = wave_rx.recv() => m,
            _ = tokio::time::sleep(remaining) => continue,
        };
        let Some(env) = env else { continue };

        // Handle events
        if env.meta.kind != MessageKind::Event { continue }
        let Some(kind) = event_type(&env) else { continue };

        let task_id = env.meta.channel.rsplit_once(".task.")
            .map(|(_, id)| id.to_string())
            .or_else(|| env.payload.get("task_id").and_then(|v| v.as_str()).map(String::from));
        let Some(task_id) = task_id else { continue };
        if !task_map.contains_key(&task_id) { continue }

        match kind {
            event_types::COMPLETED => {
                let result = env.payload.get("data").and_then(|d| d.get("result")).and_then(|v| v.as_str()).unwrap_or("");
                api.request("wave.update_task_status", serde_json::json!({
                    "wave_id": wave_id, "task_id": &task_id, "status": "done", "result": result
                })).await?;
                if let Some(e) = task_map.get_mut(&task_id) { e.status = "done".into(); e.result = Some(result.into()); }
                completed.insert(task_id);
            }
            event_types::ERROR => {
                let err = env.payload.get("data").and_then(|d| d.get("error")).and_then(|v| v.as_str()).unwrap_or("failed");
                api.request("wave.update_task_status", serde_json::json!({
                    "wave_id": wave_id, "task_id": &task_id, "status": "failed", "result": err
                })).await?;
                if let Some(e) = task_map.get_mut(&task_id) { e.status = "failed".into(); }
                failed = true;
            }
            _ => {}
        }
    }
}

async fn start_task(client: &HubClient, wave_id: &str, task: &WaveTaskRecord) -> Result<()> {
    let task_channel = subjects::wave_task_channel_name(wave_id, &task.task_id);
    let mut payload = serde_json::json!({
        "action": "session_start", "session_id": &task.task_id, "wave_id": wave_id,
        "channel": &task_channel, "prompt": &task.goal, "write_scope": &task.write_scope,
    });
    if let Some(ref cmd) = task.verify_cmd { payload["verify_cmd"] = serde_json::json!(cmd); }
    if let Some(ref path) = task.handoff_path { payload["handoff_path"] = serde_json::json!(path); }
    let env = nats_hub::Envelope::new(client.identity(), &task_channel, MessageKind::Message, payload).to(&task.worker);
    client.send(&env).await.context("start wave task")?;
    Ok(())
}

async fn show_status(api: &ApiClient, wave_id: &str) -> Result<()> {
    let resp = api.request("wave.get", serde_json::json!({"wave_id": wave_id})).await?;
    let wave: WaveRecord = resp.get("wave").and_then(|w| serde_json::from_value(w.clone()).ok())
        .context("wave not found")?;
    let tasks: Vec<WaveTaskRecord> = api.request("wave.list_tasks", serde_json::json!({"wave_id": wave_id}))
        .await?.get("tasks").and_then(|t| serde_json::from_value(t.clone()).ok()).unwrap_or_default();

    let done = tasks.iter().filter(|t| t.status == "done").count();
    let pct = if tasks.is_empty() { 0 } else { (done * 100) / tasks.len() };

    println!("wave_id:      {}", wave.wave_id);
    println!("goal:         {}", wave.goal);
    println!("status:       {}", wave.status);
    println!("orchestrator: {}", wave.orchestrator);
    println!("progress:     {done}/{} tasks ({pct}%)\n", tasks.len());

    if tasks.is_empty() { println!("(no tasks)"); return Ok(()); }

    let id_w = tasks.iter().map(|t| t.task_id.len()).max().unwrap_or(8).max(7);
    let worker_w = tasks.iter().map(|t| t.worker.len()).max().unwrap_or(6).max(6);
    println!("{:<id_w$}  {:<worker_w$}  {:<8}  DEPS", "TASK_ID", "WORKER", "STATUS");
    println!("{}", "-".repeat(id_w + worker_w + 20));
    for t in &tasks {
        let deps = if t.dependencies.is_empty() { "-".to_string() } else { t.dependencies.join(",") };
        println!("{:<id_w$}  {:<worker_w$}  {:<8}  {deps}", t.task_id, t.worker, t.status);
    }
    Ok(())
}

async fn close_wave(api: &ApiClient, nats_url: &str, wave_id: &str, from: &str) -> Result<()> {
    let resp = api.request("wave.list_tasks", serde_json::json!({"wave_id": wave_id})).await?;
    let tasks: Vec<WaveTaskRecord> = resp.get("tasks").and_then(|t| serde_json::from_value(t.clone()).ok()).unwrap_or_default();
    let final_status = evaluate_merge_gate(&tasks);
    if final_status == "running" { bail!("wave '{wave_id}' still running"); }

    api.request("wave.update_status", serde_json::json!({"wave_id": wave_id, "status": final_status})).await?;

    let client = HubClient::connect(nats_url, from).await?;
    let wave_channel = subjects::wave_channel_name(wave_id);
    client.send_message(&wave_channel, serde_json::json!({"action": "wave_close", "wave_id": wave_id, "status": final_status})).await?;
    client.drain().await?;
    eprintln!("[hub-wave] closed {wave_id} as {final_status}");
    Ok(())
}

async fn list_waves(api: &ApiClient, status: Option<String>) -> Result<()> {
    let params = match &status {
        Some(s) => serde_json::json!({"status": s}),
        None => serde_json::json!({}),
    };
    let resp = api.request("wave.list", params).await?;
    let waves: Vec<WaveRecord> = resp.get("waves").and_then(|w| serde_json::from_value(w.clone()).ok()).unwrap_or_default();

    if waves.is_empty() { println!("(no waves match)"); return Ok(()); }
    let id_w = waves.iter().map(|w| w.wave_id.len()).max().unwrap_or(8).max(7);
    println!("{:<id_w$}  {:<10}  GOAL", "WAVE_ID", "STATUS");
    println!("{}", "-".repeat(id_w + 20));
    for w in &waves {
        let goal = if w.goal.len() > 60 { format!("{}…", &w.goal[..57]) } else { w.goal.clone() };
        println!("{:<id_w$}  {:<10}  {goal}", w.wave_id, w.status);
    }
    println!("\n{} wave(s)", waves.len());
    Ok(())
}
