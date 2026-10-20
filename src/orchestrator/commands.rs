//! Commands forwarded from the query API (`wave.spawn`, `wave.cancel`,
//! `wave.update_task_status` on a running wave).

use serde_json::{json, Value};
use tokio::time::Instant;
use tracing::{info, warn};

use crate::wave::{validate_id, validate_tasks, WaveTaskInput};

use super::api::OrchCommand;
use super::engine::WaveOrchestrator;
use super::state::{
    clamp_timeout_secs, deadline_after, is_wave_terminal, snapshot_json, task_to_input, WaveRt,
};

impl WaveOrchestrator {
    pub(super) async fn on_command(&mut self, cmd: OrchCommand) {
        match cmd {
            OrchCommand::Spawn {
                wave_id,
                timeout_secs,
                reply,
            } => {
                let _ = reply.send(self.spawn_wave(&wave_id, timeout_secs).await);
            }
            OrchCommand::Cancel { wave_id, reply } => {
                let _ = reply.send(self.cancel_wave(&wave_id).await);
            }
            OrchCommand::UpdateTask {
                wave_id,
                task_id,
                status,
                result,
                reply,
            } => {
                let r = self.update_task(&wave_id, &task_id, &status, result).await;
                let _ = reply.send(r);
            }
        }
    }

    pub(super) async fn spawn_wave(
        &mut self,
        wave_id: &str,
        timeout_secs: u64,
    ) -> Result<Value, String> {
        if self.waves.contains_key(wave_id) {
            // Already running — an idempotent spawn returns the live snapshot.
            return self.snapshot(wave_id).await;
        }
        validate_id("wave_id", wave_id).map_err(|e| format!("{e:#}"))?;
        let timeout_secs = clamp_timeout_secs(timeout_secs);
        let wave = self
            .storage
            .get_wave(wave_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("wave '{wave_id}' not found"))?;
        match wave.status.as_str() {
            "pending" | "created" | "active" | "planned" => {}
            other => {
                return Err(format!(
                    "wave '{wave_id}' is '{other}' — only a pending wave can be spawned"
                ))
            }
        }
        let tasks = self
            .storage
            .list_wave_tasks(wave_id)
            .await
            .map_err(|e| e.to_string())?;
        if tasks.is_empty() {
            return Err(format!("wave '{wave_id}' has no tasks"));
        }
        let inputs: Vec<WaveTaskInput> = tasks.iter().map(task_to_input).collect();
        validate_tasks(&inputs).map_err(|e| format!("{e:#}"))?;

        self.seed_liveness().await;

        let mut wave = wave;
        wave.status = "running".to_string();
        if !wave.metadata.is_object() {
            wave.metadata = json!({});
        }
        wave.metadata["timeout_secs"] = json!(timeout_secs);
        self.storage
            .create_wave(wave.clone())
            .await
            .map_err(|e| e.to_string())?;

        let rt = WaveRt::new(&wave, tasks, deadline_after(Instant::now(), timeout_secs));
        let running_ids = rt.running_ids();
        self.waves.insert(wave_id.to_string(), rt);
        info!(%wave_id, timeout_secs, "wave spawned");
        self.publish_wave_event(
            wave_id,
            "wave_started",
            json!({"wave_id": wave_id, "timeout_secs": timeout_secs}),
        )
        .await;
        for task_id in running_ids {
            let task = self.waves[wave_id].tasks[&task_id].clone();
            if let Err(e) = self.send_dispatch(wave_id, &task).await {
                warn!(error = %e, %wave_id, %task_id, "re-dispatch failed");
            }
        }
        self.dispatch_ready(wave_id).await;
        self.snapshot(wave_id).await
    }

    pub(super) async fn cancel_wave(&mut self, wave_id: &str) -> Result<Value, String> {
        let wave = self
            .storage
            .get_wave(wave_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("wave '{wave_id}' not found"))?;
        // Terminal waves (including `completed`) are final.
        if is_wave_terminal(&wave.status) {
            return Err(format!("wave '{wave_id}' is already {}", wave.status));
        }
        self.shutdown_wave(wave_id, "cancelled", "cancelled by request")
            .await;
        self.snapshot(wave_id).await
    }

    /// `wave.update_task_status` for a wave this orchestrator drives.
    async fn update_task(
        &mut self,
        wave_id: &str,
        task_id: &str,
        status: &str,
        result: Option<String>,
    ) -> Result<Value, String> {
        let Some(current) = self.task_status(wave_id, task_id).map(String::from) else {
            return Err(if self.waves.contains_key(wave_id) {
                format!("task '{task_id}' not found in wave '{wave_id}'")
            } else {
                format!("wave '{wave_id}' is not running in this orchestrator")
            });
        };
        if current == status {
            return Ok(json!({"updated": false, "status": current}));
        }
        if current != "running" {
            return Err(format!(
                "task '{task_id}' is '{current}' — while the wave runs only a running \
                 task's status can change (terminal states are final)"
            ));
        }
        match status {
            "done" => {
                self.complete_task(wave_id, task_id, result.unwrap_or_default())
                    .await
            }
            "failed" => {
                let reason = result.unwrap_or_else(|| "marked failed via API".to_string());
                self.fail_task(wave_id, task_id, reason, true).await
            }
            "cancelled" => self.cancel_task(wave_id, task_id, true).await,
            other => {
                return Err(format!(
                    "cannot set a running task to '{other}' (allowed: done, failed, cancelled)"
                ))
            }
        }
        Ok(json!({"updated": true, "status": status}))
    }

    pub(super) async fn snapshot(&self, wave_id: &str) -> Result<Value, String> {
        let wave = self
            .storage
            .get_wave(wave_id)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("wave '{wave_id}' not found"))?;
        let tasks = self
            .storage
            .list_wave_tasks(wave_id)
            .await
            .map_err(|e| e.to_string())?;
        Ok(snapshot_json(&wave, &tasks))
    }
}
