//! Dependency dispatch, wave finalization, fail-fast teardown, and the
//! `session_start` / cancel DMs sent to workers.

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::json;
use tokio::time::Instant;
use tracing::{info, warn};

use crate::client::HubClient;
use crate::protocol::{subjects, Envelope, MessageKind};
use crate::storage::WaveTaskRecord;
use crate::wave::evaluate_merge_gate;

use super::engine::WaveOrchestrator;
use super::state::is_terminal;

impl WaveOrchestrator {
    /// DM `session_start` to every pending task whose deps are all done.
    pub(super) async fn dispatch_ready(&mut self, wave_id: &str) {
        let ready: Vec<WaveTaskRecord> = {
            let Some(rt) = self.waves.get(wave_id) else {
                return;
            };
            rt.tasks
                .values()
                .filter(|t| t.status == "pending")
                .filter(|t| t.dependencies.iter().all(|d| rt.done.contains(d)))
                .cloned()
                .collect()
        };
        for task in ready {
            let task_id = task.task_id.clone();
            if let Err(e) = send_session_start(&self.client, wave_id, &task).await {
                warn!(error = %e, %wave_id, %task_id, "task dispatch failed");
                self.fail_task(wave_id, &task_id, format!("dispatch failed: {e:#}"))
                    .await;
                return; // the wave is dead; stop dispatching
            }
            let row = {
                let Some(rt) = self.waves.get_mut(wave_id) else {
                    return;
                };
                let Some(t) = rt.tasks.get_mut(&task_id) else {
                    continue;
                };
                t.status = "running".to_string();
                t.started_at = Some(Utc::now());
                rt.dispatched.insert(task_id.clone(), Instant::now());
                t.clone()
            };
            self.persist_task(&row).await;
            info!(%wave_id, %task_id, worker = %row.worker, "dispatched wave task");
            self.publish_wave_event(
                wave_id,
                "task_dispatched",
                json!({"task_id": task_id, "worker": row.worker}),
            )
            .await;
        }
    }

    /// If every task is terminal, write the wave's final status + event.
    pub(super) async fn check_terminal(&mut self, wave_id: &str) {
        let (all_terminal, status) = {
            let Some(rt) = self.waves.get(wave_id) else {
                return;
            };
            let all = rt.tasks.values().all(|t| is_terminal(&t.status));
            let status = evaluate_merge_gate(&rt.tasks.values().cloned().collect::<Vec<_>>());
            (all, status)
        };
        if all_terminal {
            self.finalize_wave(wave_id, status, None).await;
        }
    }

    pub(super) async fn finalize_wave(
        &mut self,
        wave_id: &str,
        status: &str,
        reason: Option<&str>,
    ) {
        if let Err(e) = self.storage.update_wave_status(wave_id, status).await {
            warn!(error = %e, %wave_id, %status, "failed to finalize wave");
        }
        info!(%wave_id, %status, ?reason, "wave finished");
        let mut data = json!({"wave_id": wave_id, "status": status});
        if let Some(r) = reason {
            data["reason"] = json!(r);
        }
        self.publish_wave_event(wave_id, &format!("wave_{status}"), data)
            .await;
        self.waves.remove(wave_id);
    }

    /// Fail-fast teardown: cancel every non-terminal task (DM §4.2 cancels
    /// to running tasks' workers), set the wave's final status, drop it.
    /// Also used for `wave.cancel` and wave timeout.
    pub(super) async fn shutdown_wave(&mut self, wave_id: &str, status: &str, reason: &str) {
        let mut rows = Vec::new();
        let mut dm_targets: Vec<WaveTaskRecord> = Vec::new();
        if let Some(rt) = self.waves.get_mut(wave_id) {
            for t in rt.tasks.values_mut() {
                if is_terminal(&t.status) {
                    continue;
                }
                if t.status == "running" {
                    dm_targets.push(t.clone());
                }
                t.status = "cancelled".to_string();
                t.completed_at = Some(Utc::now());
                t.result = Some(reason.to_string());
                rows.push(t.clone());
            }
        } else {
            // Not tracked (e.g. cancelling a pending wave): fall back to the
            // stored rows so tasks still end up cancelled.
            match self.storage.list_wave_tasks(wave_id).await {
                Ok(tasks) => {
                    for mut t in tasks {
                        if is_terminal(&t.status) {
                            continue;
                        }
                        t.status = "cancelled".to_string();
                        t.completed_at = Some(Utc::now());
                        t.result = Some(reason.to_string());
                        rows.push(t);
                    }
                }
                Err(e) => warn!(error = %e, %wave_id, "wave shutdown: failed to load tasks"),
            }
        }
        for row in &rows {
            self.persist_task(row).await;
        }
        for task in &dm_targets {
            send_cancel_dm(&self.client, wave_id, task).await;
        }
        self.finalize_wave(wave_id, status, Some(reason)).await;
    }
}

/// The `session_start` DM that tells a worker to run one wave task.
pub(super) async fn send_session_start(
    client: &HubClient,
    wave_id: &str,
    task: &WaveTaskRecord,
) -> Result<()> {
    let task_channel = subjects::wave_task_channel_name(wave_id, &task.task_id);
    let mut payload = json!({
        "action": "session_start",
        "session_id": task.task_id,
        "wave_id": wave_id,
        "channel": task_channel,
        "prompt": task.goal,
        "write_scope": task.write_scope,
    });
    if let Some(ref cmd) = task.verify_cmd {
        payload["verify_cmd"] = json!(cmd);
    }
    if let Some(ref path) = task.handoff_path {
        payload["handoff_path"] = json!(path);
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
        .with_context(|| format!("session_start for task '{}'", task.task_id))
}

/// §4.2 cancel contract: DM `kind=control {"action":"cancel","task_id":…}`.
pub(super) async fn send_cancel_dm(client: &HubClient, wave_id: &str, task: &WaveTaskRecord) {
    let task_channel = subjects::wave_task_channel_name(wave_id, &task.task_id);
    let payload = json!({
        "action": "cancel",
        "task_id": task.task_id,
        "wave_id": wave_id,
    });
    let env = Envelope::new(
        client.identity(),
        &task_channel,
        MessageKind::Control,
        payload,
    )
    .to(&task.worker);
    if let Err(e) = client.send(&env).await {
        warn!(error = %e, %wave_id, task_id = %task.task_id, "cancel DM failed");
    }
}
