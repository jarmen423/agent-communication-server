//! Resume after a hub-server restart: reload `running` waves from storage
//! and re-dispatch their in-flight tasks.

use anyhow::Result;
use tokio::time::Instant;
use tracing::{info, warn};

use crate::storage::WaveRecord;
use crate::wave::evaluate_merge_gate;

use super::engine::WaveOrchestrator;
use super::state::{
    clamp_timeout_secs, deadline_after, instant_from_datetime, is_terminal, WaveRt,
    DEFAULT_TIMEOUT_SECS,
};

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
    /// in-flight tasks (at-least-once, same session id; workers dedupe a
    /// `session_start` for a turn that is still running — contract C3).
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

        // A stored timeout is clamped, never trusted: an out-of-range value
        // (written before the API clamped it) would overflow the deadline.
        let stored = wave.metadata.get("timeout_secs").and_then(|v| v.as_u64());
        let timeout_secs = clamp_timeout_secs(stored.unwrap_or(DEFAULT_TIMEOUT_SECS));
        if stored.is_some_and(|s| s != timeout_secs) {
            warn!(%wave_id, stored = ?stored, timeout_secs, "clamped out-of-range stored wave timeout");
        }
        // Keep the original clock: the deadline derives from the earliest
        // started task, not from "now" (restart doesn't extend the wave).
        let start = tasks
            .iter()
            .filter_map(|t| t.started_at)
            .min()
            .map(instant_from_datetime)
            .unwrap_or_else(Instant::now);
        let deadline = deadline_after(start, timeout_secs);

        let rt = WaveRt::new(&wave, tasks, deadline);
        let running_ids = rt.running_ids();
        self.waves.insert(wave_id.clone(), rt);
        info!(%wave_id, running = running_ids.len(), "resumed wave");

        // Re-dispatch in-flight tasks, then any newly-ready pending ones.
        for task_id in running_ids {
            let task = self.waves[&wave_id].tasks[&task_id].clone();
            if let Err(e) = self.send_dispatch(&wave_id, &task).await {
                warn!(error = %e, %wave_id, %task_id, "re-dispatch failed");
            }
        }
        self.dispatch_ready(&wave_id).await;
        self.check_terminal(&wave_id).await;
        Ok(())
    }
}
