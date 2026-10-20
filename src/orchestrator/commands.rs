//! Commands forwarded from the query API (`wave.spawn`, `wave.cancel`).

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::Instant;
use tracing::info;

use crate::storage::WaveTaskRecord;
use crate::wave::{validate_tasks, WaveTaskInput};

use super::api::OrchCommand;
use super::dispatch::send_session_start;
use super::engine::WaveOrchestrator;
use super::state::{is_terminal, snapshot_json, task_to_input, WaveRt};

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
        wave.metadata["timeout_secs"] = json!(timeout_secs);
        let storage = self.storage.clone();
        storage.create_wave(wave).await.map_err(|e| e.to_string())?;

        let done: HashSet<String> = tasks
            .iter()
            .filter(|t| t.status == "done")
            .map(|t| t.task_id.clone())
            .collect();
        let running_ids: Vec<String> = tasks
            .iter()
            .filter(|t| t.status == "running")
            .map(|t| t.task_id.clone())
            .collect();
        let task_map: HashMap<String, WaveTaskRecord> =
            tasks.into_iter().map(|t| (t.task_id.clone(), t)).collect();
        self.waves.insert(
            wave_id.to_string(),
            WaveRt {
                tasks: task_map,
                done,
                deadline: Instant::now() + Duration::from_secs(timeout_secs),
                dispatched: HashMap::new(),
            },
        );
        info!(%wave_id, timeout_secs, "wave spawned");
        self.publish_wave_event(
            wave_id,
            "wave_started",
            json!({"wave_id": wave_id, "timeout_secs": timeout_secs}),
        )
        .await;
        for task_id in running_ids {
            let task = self.waves[wave_id].tasks[&task_id].clone();
            if send_session_start(&self.client, wave_id, &task)
                .await
                .is_ok()
            {
                if let Some(rt) = self.waves.get_mut(wave_id) {
                    rt.dispatched.insert(task_id, Instant::now());
                }
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
        if is_terminal(&wave.status) {
            return Err(format!("wave '{wave_id}' is already {}", wave.status));
        }
        self.shutdown_wave(wave_id, "cancelled", "cancelled by request")
            .await;
        self.snapshot(wave_id).await
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
