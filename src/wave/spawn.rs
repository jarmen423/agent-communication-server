//! Wave spawn orchestration — start tasks respecting dependencies.

use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tracing::info;

use crate::client::HubClient;
use crate::events::{event_type, event_types};
use crate::protocol::{subjects, Envelope, MessageKind};
use crate::storage::{Storage, WaveTaskRecord};

/// Outcome of a wave spawn run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnOutcome {
    Completed,
    Failed,
    Timeout,
}

/// Spawn all tasks in a wave, respecting dependencies and listening for completion events.
pub async fn spawn_wave<S: Storage>(
    client: &HubClient,
    storage: &S,
    wave_id: &str,
    timeout_secs: u64,
) -> Result<SpawnOutcome> {
    let tasks = storage.list_wave_tasks(wave_id).await?;
    if tasks.is_empty() {
        bail!("no tasks found for wave '{wave_id}'");
    }

    storage.update_wave_status(wave_id, "running").await?;

    let mut task_map: HashMap<String, WaveTaskRecord> =
        tasks.into_iter().map(|t| (t.task_id.clone(), t)).collect();
    let mut completed: HashSet<String> = HashSet::new();
    let mut failed = false;

    let wave_prefix = subjects::wave_channel_name(wave_id);
    let mut event_rx = client
        .subscribe_subject(&format!("channel.{wave_prefix}.>"))
        .await?;
    // Also listen on the wave-level channel for dual-published events.
    let mut wave_rx = client.subscribe_channel(&wave_prefix).await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        if failed {
            storage.update_wave_status(wave_id, "failed").await?;
            return Ok(SpawnOutcome::Failed);
        }

        let all_terminal = task_map
            .values()
            .all(|t| t.status == "done" || t.status == "failed");
        if all_terminal {
            let any_failed = task_map.values().any(|t| t.status == "failed");
            let final_status = if any_failed { "failed" } else { "completed" };
            storage.update_wave_status(wave_id, final_status).await?;
            return Ok(if any_failed {
                SpawnOutcome::Failed
            } else {
                SpawnOutcome::Completed
            });
        }

        // Start any pending tasks whose dependencies are satisfied.
        let ready: Vec<WaveTaskRecord> = task_map
            .values()
            .filter(|t| t.status == "pending")
            .filter(|t| t.dependencies.iter().all(|d| completed.contains(d)))
            .cloned()
            .collect();

        for task in ready {
            start_task(client, wave_id, &task).await?;
            storage
                .update_wave_task_status(wave_id, &task.task_id, "running", None)
                .await?;
            if let Some(entry) = task_map.get_mut(&task.task_id) {
                entry.status = "running".to_string();
            }
            info!(wave_id, task_id = %task.task_id, worker = %task.worker, "started wave task");
        }

        let remaining = Duration::from_secs(1).min(
            deadline
                .checked_duration_since(tokio::time::Instant::now())
                .unwrap_or_default(),
        );
        if remaining.is_zero() {
            storage.update_wave_status(wave_id, "failed").await?;
            return Ok(SpawnOutcome::Timeout);
        }

        let env = tokio::select! {
            maybe = event_rx.recv() => maybe,
            maybe = wave_rx.recv() => maybe,
            _ = tokio::time::sleep(remaining) => {
                continue;
            }
        };

        let Some(env) = env else {
            continue;
        };

        handle_task_event(
            storage,
            wave_id,
            &env,
            &mut task_map,
            &mut completed,
            &mut failed,
        )
        .await?;
    }
}

async fn start_task(client: &HubClient, wave_id: &str, task: &WaveTaskRecord) -> Result<()> {
    let task_channel = subjects::wave_task_channel_name(wave_id, &task.task_id);
    let mut payload = serde_json::json!({
        "action": "session_start",
        "session_id": task.task_id,
        "wave_id": wave_id,
        "channel": task_channel,
        "prompt": task.goal,
        "write_scope": task.write_scope,
    });
    if let Some(ref cmd) = task.verify_cmd {
        payload["verify_cmd"] = serde_json::json!(cmd);
    }
    if let Some(ref path) = task.handoff_path {
        payload["handoff_path"] = serde_json::json!(path);
    }

    let env = Envelope::new(
        client.identity(),
        &task_channel,
        MessageKind::Message,
        payload,
    )
    .to(&task.worker);
    client
        .send(&env)
        .await
        .context("failed to start wave task")?;
    Ok(())
}

async fn handle_task_event<S: Storage>(
    storage: &S,
    wave_id: &str,
    env: &Envelope,
    task_map: &mut HashMap<String, WaveTaskRecord>,
    completed: &mut HashSet<String>,
    failed: &mut bool,
) -> Result<()> {
    if env.meta.kind != MessageKind::Event {
        return Ok(());
    }

    let kind = match event_type(env) {
        Some(k) => k,
        None => return Ok(()),
    };

    // Extract task_id from channel name: wave.<wave>.task.<task_id>
    let task_id = env
        .meta
        .channel
        .rsplit_once(".task.")
        .map(|(_, id)| id.to_string())
        .or_else(|| {
            env.payload
                .get("task_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        });

    let Some(task_id) = task_id else {
        return Ok(());
    };

    if !task_map.contains_key(&task_id) {
        return Ok(());
    }

    match kind {
        event_types::COMPLETED => {
            let result = env
                .payload
                .get("data")
                .and_then(|d| d.get("result"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            storage
                .update_wave_task_status(wave_id, &task_id, "done", Some(result))
                .await?;
            if let Some(entry) = task_map.get_mut(&task_id) {
                entry.status = "done".to_string();
                entry.result = Some(result.to_string());
            }
            completed.insert(task_id);
        }
        event_types::ERROR => {
            let err = env
                .payload
                .get("data")
                .and_then(|d| d.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("task failed");
            storage
                .update_wave_task_status(wave_id, &task_id, "failed", Some(err))
                .await?;
            if let Some(entry) = task_map.get_mut(&task_id) {
                entry.status = "failed".to_string();
            }
            *failed = true;
        }
        _ => {}
    }

    Ok(())
}

/// Evaluate merge gate for a wave (all tasks done, none failed).
pub fn evaluate_merge_gate(tasks: &[WaveTaskRecord]) -> &'static str {
    if tasks.iter().any(|t| t.status == "failed") {
        "failed"
    } else if tasks.iter().all(|t| t.status == "done") {
        "completed"
    } else {
        "running"
    }
}
