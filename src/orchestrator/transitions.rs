//! Task transitions. Each persists the task before returning.

use chrono::Utc;
use serde_json::json;
use tracing::{debug, info};

use crate::storage::WaveTaskRecord;

use super::engine::WaveOrchestrator;
use super::state::is_terminal;

impl WaveOrchestrator {
    pub(super) async fn complete_task(&mut self, wave_id: &str, task_id: &str, result: String) {
        let Some(row) = ({
            let Some(rt) = self.waves.get_mut(wave_id) else {
                return;
            };
            let Some(task) = rt.tasks.get_mut(task_id) else {
                return;
            };
            if is_terminal(&task.status) {
                return;
            }
            task.status = "done".to_string();
            task.completed_at = Some(Utc::now());
            task.result = Some(result.clone());
            if task.verify_cmd.is_some() && task.verify_result.is_none() {
                // Finished without the expected verify_passed milestone.
                task.verify_result = Some("missing".to_string());
            }
            rt.done.insert(task_id.to_string());
            Some(task.clone())
        }) else {
            return;
        };
        self.persist_task(&row).await;
        info!(%wave_id, %task_id, "wave task done");
        self.publish_wave_event(
            wave_id,
            "task_completed",
            json!({"task_id": task_id, "result": row.result}),
        )
        .await;
        self.dispatch_ready(wave_id).await;
        self.check_terminal(wave_id).await;
    }

    /// A task failure is fail-fast: the whole wave goes down with it.
    pub(super) async fn fail_task(&mut self, wave_id: &str, task_id: &str, reason: String) {
        let Some(row) = ({
            let Some(rt) = self.waves.get_mut(wave_id) else {
                return;
            };
            let Some(task) = rt.tasks.get_mut(task_id) else {
                return;
            };
            if is_terminal(&task.status) {
                return;
            }
            task.status = "failed".to_string();
            task.completed_at = Some(Utc::now());
            task.result = Some(reason.clone());
            if task.verify_cmd.is_some() && task.verify_result.is_none() {
                task.verify_result = Some("failed".to_string());
            }
            Some(task.clone())
        }) else {
            return;
        };
        self.persist_task(&row).await;
        info!(%wave_id, %task_id, %reason, "wave task failed");
        self.publish_wave_event(
            wave_id,
            "task_failed",
            json!({"task_id": task_id, "error": reason}),
        )
        .await;
        self.shutdown_wave(
            wave_id,
            "failed",
            &format!("task {task_id} failed: {reason}"),
        )
        .await;
    }

    /// Worker reported `status: "cancelled"` (§4.2). The task is cancelled;
    /// pending tasks that can now never run cascade to `cancelled` too.
    pub(super) async fn worker_cancelled(&mut self, wave_id: &str, task_id: &str) {
        let Some(row) = ({
            let Some(rt) = self.waves.get_mut(wave_id) else {
                return;
            };
            let Some(task) = rt.tasks.get_mut(task_id) else {
                return;
            };
            if is_terminal(&task.status) {
                return;
            }
            task.status = "cancelled".to_string();
            task.completed_at = Some(Utc::now());
            task.result = Some("cancelled".to_string());
            Some(task.clone())
        }) else {
            return;
        };
        self.persist_task(&row).await;
        self.publish_wave_event(wave_id, "task_cancelled", json!({"task_id": task_id}))
            .await;
        self.cancel_unsatisfiable(wave_id).await;
        self.check_terminal(wave_id).await;
    }

    pub(super) async fn record_verify(&mut self, wave_id: &str, task_id: &str, result: &str) {
        let row = {
            let Some(rt) = self.waves.get_mut(wave_id) else {
                return;
            };
            let Some(task) = rt.tasks.get_mut(task_id) else {
                return;
            };
            task.verify_result = Some(result.to_string());
            task.clone()
        };
        self.persist_task(&row).await;
        debug!(%wave_id, %task_id, %result, "verify result recorded");
    }

    /// Pending tasks whose dependencies can never complete (a dep is
    /// `cancelled` or `failed`) are cancelled, transitively.
    pub(super) async fn cancel_unsatisfiable(&mut self, wave_id: &str) {
        loop {
            let doomed: Vec<WaveTaskRecord> = {
                let Some(rt) = self.waves.get(wave_id) else {
                    return;
                };
                rt.tasks
                    .values()
                    .filter(|t| t.status == "pending")
                    .filter(|t| {
                        t.dependencies.iter().any(|d| {
                            rt.tasks
                                .get(d)
                                .map(|dep| matches!(dep.status.as_str(), "cancelled" | "failed"))
                                .unwrap_or(false)
                        })
                    })
                    .cloned()
                    .collect()
            };
            if doomed.is_empty() {
                return;
            }
            for task in doomed {
                let row = {
                    let Some(rt) = self.waves.get_mut(wave_id) else {
                        return;
                    };
                    let Some(t) = rt.tasks.get_mut(&task.task_id) else {
                        continue;
                    };
                    t.status = "cancelled".to_string();
                    t.completed_at = Some(Utc::now());
                    t.result = Some("dependency will never complete".to_string());
                    t.clone()
                };
                self.persist_task(&row).await;
                self.publish_wave_event(
                    wave_id,
                    "task_cancelled",
                    json!({"task_id": row.task_id, "reason": "dependency will never complete"}),
                )
                .await;
            }
        }
    }
}
