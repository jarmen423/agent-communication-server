//! Resume after a hub-server restart: reload `running` waves from storage
//! and re-dispatch their in-flight tasks.

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::Instant;
use tracing::{info, warn};

use crate::storage::{WaveRecord, WaveTaskRecord};
use crate::wave::evaluate_merge_gate;

use super::dispatch::send_session_start;
use super::engine::WaveOrchestrator;
use super::state::{instant_from_datetime, is_terminal, WaveRt, DEFAULT_TIMEOUT_SECS};

impl WaveOrchestrator {
    pub(super) async fn resume(&mut self) -> Result<()> {
        self.seed_liveness().await;
        let running = self.storage.list_waves(Some("running")).await?;
        for wave in running {
            let wave_id = wave.wave_id.clone();
            if let Err(e) = self.adopt_wave(wave).await {
                warn!(error = %e, %wave_id, "failed to resume wave");
            }
        }
        Ok(())
    }

    /// Rebuild runtime state for a `running` wave and re-dispatch its
    /// in-flight tasks (at-least-once; workers dedupe or restart the work).
    pub(super) async fn adopt_wave(&mut self, wave: WaveRecord) -> Result<()> {
        let wave_id = wave.wave_id.clone();
        let tasks = self.storage.list_wave_tasks(&wave_id).await?;
        if tasks.is_empty() {
            warn!(%wave_id, "running wave has no tasks; marking failed");
            self.storage.update_wave_status(&wave_id, "failed").await?;
            return Ok(());
        }
        if tasks.iter().all(|t| is_terminal(&t.status)) {
            // Crashed between the last task update and the wave write.
            let status = evaluate_merge_gate(&tasks).to_string();
            self.storage.update_wave_status(&wave_id, &status).await?;
            info!(%wave_id, %status, "finalized orphaned wave on resume");
            return Ok(());
        }

        let timeout_secs = wave
            .metadata
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        // Keep the original clock: the deadline derives from the earliest
        // started task, not from "now" (restart doesn't extend the wave).
        let deadline = tasks
            .iter()
            .filter_map(|t| t.started_at)
            .min()
            .map(|t| instant_from_datetime(t) + Duration::from_secs(timeout_secs))
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(timeout_secs));

        let done: HashSet<String> = tasks
            .iter()
            .filter(|t| t.status == "done")
            .map(|t| t.task_id.clone())
            .collect();
        let task_map: HashMap<String, WaveTaskRecord> =
            tasks.into_iter().map(|t| (t.task_id.clone(), t)).collect();
        let running_ids: Vec<String> = task_map
            .values()
            .filter(|t| t.status == "running")
            .map(|t| t.task_id.clone())
            .collect();
        self.waves.insert(
            wave_id.clone(),
            WaveRt {
                tasks: task_map,
                done,
                deadline,
                dispatched: HashMap::new(),
            },
        );
        info!(%wave_id, running = running_ids.len(), "resumed wave");

        // Re-dispatch in-flight tasks, then any newly-ready pending ones.
        for task_id in running_ids {
            let task = self.waves[&wave_id].tasks[&task_id].clone();
            if let Err(e) = send_session_start(&self.client, &wave_id, &task).await {
                warn!(error = %e, %wave_id, %task_id, "re-dispatch failed");
                continue;
            }
            if let Some(rt) = self.waves.get_mut(&wave_id) {
                rt.dispatched.insert(task_id, Instant::now());
            }
        }
        self.dispatch_ready(&wave_id).await;
        self.check_terminal(&wave_id).await;
        Ok(())
    }
}
