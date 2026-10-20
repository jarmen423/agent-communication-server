//! Dependency dispatch, task release (cancel / `session_close`), wave
//! finalization and fail-fast teardown.

use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::json;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::storage::{SessionRecord, WaveTaskRecord};
use crate::wave::{evaluate_merge_gate, wave_task_session_id};

use super::engine::WaveOrchestrator;
use super::messages;
use super::state::{is_terminal, DISPATCH_IDS_KEY};

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
            if let Err(e) = self.send_dispatch(wave_id, &task).await {
                warn!(error = %e, %wave_id, %task_id, "task dispatch failed");
                self.fail_task(wave_id, &task_id, format!("dispatch failed: {e:#}"), false)
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

    /// Send (or re-send) `session_start` for `task` and remember its
    /// envelope id as the task's cancel id (C2), in memory and in
    /// `wave.metadata.dispatch_ids`. The task's session record is created
    /// first, so the worker can `session.set_backend_ctx` on it.
    pub(super) async fn send_dispatch(
        &mut self,
        wave_id: &str,
        task: &WaveTaskRecord,
    ) -> Result<()> {
        self.ensure_task_session(wave_id, task).await;
        let env = messages::session_start(self.client.identity(), wave_id, task);
        self.client
            .send(&env)
            .await
            .with_context(|| format!("session_start for task '{}'", task.task_id))?;
        let ids = {
            let Some(rt) = self.waves.get_mut(wave_id) else {
                return Ok(());
            };
            rt.dispatched.insert(task.task_id.clone(), Instant::now());
            rt.dispatch_ids
                .insert(task.task_id.clone(), env.meta.id.clone());
            json!(rt.dispatch_ids)
        };
        if let Err(e) = self
            .storage
            .set_wave_metadata_key(wave_id, DISPATCH_IDS_KEY, ids)
            .await
        {
            warn!(error = %e, %wave_id, "failed to persist dispatch ids");
        }
        Ok(())
    }

    /// Wave tasks run in session `<wave_id>--<task_id>` (C4). Give it a
    /// session record so `session.get` / `session.set_backend_ctx` work for
    /// wave tasks. Its `orchestrator` is the orchestrator's own identity —
    /// the sender of every `session_start` / `session_close` / cancel DM —
    /// because workers take a resumed session's owner from that field and
    /// accept session messages only from it. The wave's owner is kept in
    /// `metadata.wave_owner`. An existing record is kept as is (it may hold
    /// `backend_ctx`).
    async fn ensure_task_session(&self, wave_id: &str, task: &WaveTaskRecord) {
        let session_id = wave_task_session_id(wave_id, &task.task_id);
        match self.storage.get_session(&session_id).await {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(e) => {
                warn!(error = %e, %session_id, "failed to look up wave task session");
                return;
            }
        }
        let owner = self
            .waves
            .get(wave_id)
            .map(|rt| rt.owner.clone())
            .unwrap_or_default();
        let now = Utc::now();
        let record = SessionRecord {
            session_id: session_id.clone(),
            orchestrator: self.client.identity().to_string(),
            worker: task.worker.clone(),
            status: "active".to_string(),
            cwd: None,
            model: None,
            provider: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
            metadata: json!({
                "kind": "wave_task",
                "wave_id": wave_id,
                "task_id": task.task_id,
                "wave_owner": owner,
            }),
            backend_ctx: None,
        };
        if let Err(e) = self.storage.create_session(record).await {
            warn!(error = %e, %session_id, "failed to create wave task session");
        }
    }

    /// A dispatched task reached a terminal state: optionally DM the §4.2
    /// cancel (the worker may still be running it), then `session_close`,
    /// and close the session record.
    pub(super) async fn release_task(&self, wave_id: &str, task: &WaveTaskRecord, cancel: bool) {
        let identity = self.client.identity();
        if cancel {
            let dispatch_id = self
                .waves
                .get(wave_id)
                .and_then(|rt| rt.dispatch_ids.get(&task.task_id))
                .cloned()
                .unwrap_or_else(|| task.task_id.clone());
            let env = messages::cancel(identity, wave_id, task, &dispatch_id);
            if let Err(e) = self.client.send(&env).await {
                warn!(error = %e, %wave_id, task_id = %task.task_id, "cancel DM failed");
            }
        }
        let env = messages::session_close(identity, wave_id, task);
        if let Err(e) = self.client.send(&env).await {
            warn!(error = %e, %wave_id, task_id = %task.task_id, "session_close DM failed");
        }
        let session_id = wave_task_session_id(wave_id, &task.task_id);
        if let Err(e) = self
            .storage
            .update_session_status(&session_id, "closed")
            .await
        {
            debug!(error = %e, %session_id, "failed to close wave task session");
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
        self.prune_liveness();
    }

    /// Fail-fast teardown: cancel every non-terminal task (running tasks'
    /// workers get the §4.2 cancel DM and `session_close`), set the wave's
    /// final status, drop it. Also used for `wave.cancel` and wave timeout.
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
            self.release_task(wave_id, task, true).await;
        }
        self.finalize_wave(wave_id, status, Some(reason)).await;
    }
}
