//! The orchestrator event loop: wave subscriptions, worker liveness,
//! persisted task transitions, dependency dispatch, and restart resume.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::client::HubClient;
use crate::events::{event_data, event_payload, event_type, event_types};
use crate::protocol::{subjects, Envelope, MessageKind};
use crate::storage::{AgentFilter, Storage, WaveRecord, WaveTaskRecord};
use crate::wave::{evaluate_merge_gate, validate_tasks, WaveTaskInput};

use super::api::{install_handle, OrchCommand, OrchestratorHandle};

/// Sender identity stamped on orchestration envelopes (task dispatches,
/// cancel DMs, wave progress events).
pub const ORCHESTRATOR_IDENTITY: &str = "hub-orchestrator";
/// Default liveness TTL: a worker silent longer loses its running tasks.
pub const DEFAULT_LIVENESS_SECS: u64 = 90;
/// Fallback wave timeout when spawn passes none and the record lacks one.
const DEFAULT_TIMEOUT_SECS: u64 = 3600;

fn is_terminal(status: &str) -> bool {
    matches!(status, "done" | "failed" | "cancelled")
}

/// Read a payload field as text (strings pass through; other JSON is
/// stringified) — task results are free-form per the reply contract.
fn json_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// `wave.<id>` → (id, None); `wave.<id>.task.<tid>` → (id, Some(tid)).
fn parse_wave_channel(channel: &str) -> Option<(String, Option<String>)> {
    let rest = channel.strip_prefix("wave.")?;
    match rest.split_once(".task.") {
        Some((wid, tid)) if !wid.is_empty() && !tid.is_empty() => {
            Some((wid.to_string(), Some(tid.to_string())))
        }
        None if !rest.contains('.') => Some((rest.to_string(), None)),
        _ => None,
    }
}

fn instant_from_datetime(dt: DateTime<Utc>) -> Instant {
    let age = (Utc::now() - dt).to_std().unwrap_or_default();
    Instant::now().checked_sub(age).unwrap_or_else(Instant::now)
}

/// JSON shape shared by `wave.spawn` / `wave.status` / `wave.cancel`.
pub fn snapshot_json(wave: &WaveRecord, tasks: &[WaveTaskRecord]) -> Value {
    let mut by_status: HashMap<String, u64> = HashMap::new();
    for t in tasks {
        *by_status.entry(t.status.clone()).or_default() += 1;
    }
    json!({
        "wave": wave,
        "tasks": tasks,
        "summary": {
            "total": tasks.len(),
            "done": tasks.iter().filter(|t| t.status == "done").count(),
            "by_status": by_status,
            "merge_gate": evaluate_merge_gate(tasks),
        },
    })
}

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// How long a worker may stay silent (no heartbeat, no wave-channel
    /// envelope) before its running task is failed.
    pub liveness_ttl: Duration,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            liveness_ttl: Duration::from_secs(DEFAULT_LIVENESS_SECS),
        }
    }
}

/// Live (non-persisted) per-wave state. Persisted state always wins: the
/// task map is seeded from storage and every mutation is written back.
struct WaveRt {
    tasks: HashMap<String, WaveTaskRecord>,
    done: HashSet<String>,
    deadline: Instant,
    /// task_id → last instant its `session_start` was sent. Liveness
    /// baseline for a worker never heard from.
    dispatched: HashMap<String, Instant>,
}

pub struct WaveOrchestrator {
    storage: Arc<dyn Storage>,
    client: HubClient,
    config: OrchestratorConfig,
    cmd_rx: mpsc::Receiver<OrchCommand>,
    waves: HashMap<String, WaveRt>,
    /// identity → most recent proof of life (heartbeat or wave-channel
    /// envelope carrying that `meta.from`).
    last_seen: HashMap<String, Instant>,
}

impl WaveOrchestrator {
    /// Connect to NATS, install the process-wide API handle, and spawn the
    /// run loop. Returns the command handle (also via `orchestrator_handle()`).
    pub async fn start(
        storage: Arc<dyn Storage>,
        nats_url: &str,
        config: OrchestratorConfig,
    ) -> Result<OrchestratorHandle> {
        let client = HubClient::connect(nats_url, ORCHESTRATOR_IDENTITY)
            .await
            .context("orchestrator: NATS connect failed")?;
        let (tx, rx) = mpsc::channel(32);
        let mut orch = Self {
            storage,
            client,
            config,
            cmd_rx: rx,
            waves: HashMap::new(),
            last_seen: HashMap::new(),
        };
        install_handle(tx.clone());
        tokio::spawn(async move {
            if let Err(e) = orch.run().await {
                warn!(error = %e, "wave orchestrator exited");
            }
        });
        Ok(tx)
    }

    async fn run(&mut self) -> Result<()> {
        let mut wave_rx = self
            .client
            .subscribe_subject("channel.wave.>")
            .await
            .context("orchestrator: subscribe channel.wave.> failed")?;
        // Heartbeats arrive on the legacy `hub.presence` and on the bound
        // `hub.presence.<identity>` (contract §4.1, what workers send since
        // T1). Missing the bound form would fail live long-running tasks as
        // "silent" after the liveness TTL.
        let mut presence_rx = self
            .client
            .subscribe_subject_tagged(subjects::PRESENCE)
            .await
            .context("orchestrator: subscribe hub.presence failed")?;
        let mut bound_presence_rx = self
            .client
            .subscribe_subject_tagged(&format!("{}.*", subjects::PRESENCE))
            .await
            .context("orchestrator: subscribe hub.presence.* failed")?;

        if let Err(e) = self.resume().await {
            warn!(error = %e, "wave resume failed; starting with an empty slate");
        }

        // Sweep often enough to catch a dead worker near its TTL.
        let period =
            (self.config.liveness_ttl / 3).clamp(Duration::from_secs(1), Duration::from_secs(10));
        let mut sweep = tokio::time::interval(period);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        info!("wave orchestrator running");
        loop {
            tokio::select! {
                Some(env) = wave_rx.recv() => self.on_wave_envelope(env).await,
                Some((subj, env)) = presence_rx.recv() => self.on_presence(&subj, &env),
                Some((subj, env)) = bound_presence_rx.recv() => self.on_presence(&subj, &env),
                Some(cmd) = self.cmd_rx.recv() => self.on_command(cmd).await,
                _ = sweep.tick() => self.sweep().await,
            }
        }
    }

    // ── Resume after hub-server restart ──────────────────────────────────

    async fn resume(&mut self) -> Result<()> {
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
    async fn adopt_wave(&mut self, wave: WaveRecord) -> Result<()> {
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

    // ── Commands from the query API ──────────────────────────────────────

    async fn on_command(&mut self, cmd: OrchCommand) {
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

    async fn spawn_wave(&mut self, wave_id: &str, timeout_secs: u64) -> Result<Value, String> {
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

    async fn cancel_wave(&mut self, wave_id: &str) -> Result<Value, String> {
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

    async fn snapshot(&self, wave_id: &str) -> Result<Value, String> {
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

    // ── Wave-channel envelopes ───────────────────────────────────────────

    async fn on_wave_envelope(&mut self, env: Envelope) {
        let Some((wave_id, chan_task)) = parse_wave_channel(&env.meta.channel) else {
            return;
        };
        if !self.waves.contains_key(&wave_id) {
            return;
        }
        // Any envelope on a tracked wave's channels proves its sender alive.
        self.last_seen.insert(env.meta.from.clone(), Instant::now());
        if env.meta.from == ORCHESTRATOR_IDENTITY {
            return; // our own progress publishes echo back on the wildcard
        }

        let task_id = chan_task
            .or_else(|| {
                event_data(&env)
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .or_else(|| {
                env.payload
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            });
        let Some(task_id) = task_id else { return };

        let (worker, task_status) = {
            let Some(rt) = self.waves.get(&wave_id) else {
                return;
            };
            match rt.tasks.get(&task_id) {
                Some(t) => (t.worker.clone(), t.status.clone()),
                None => return,
            }
        };
        if is_terminal(&task_status) {
            return;
        }
        if env.meta.from != worker {
            warn!(
                %wave_id, %task_id, sender = %env.meta.from, expected = %worker,
                "ignoring wave task envelope from foreign sender"
            );
            return;
        }

        match env.meta.kind {
            MessageKind::Event => match event_type(&env) {
                Some(event_types::COMPLETED) => {
                    let result = json_text(event_data(&env).get("result"));
                    self.complete_task(&wave_id, &task_id, result).await;
                }
                Some(event_types::ERROR) => {
                    let err = json_text(event_data(&env).get("error"));
                    self.fail_task(&wave_id, &task_id, non_empty_or(err, "task failed"))
                        .await;
                }
                Some(event_types::MILESTONE) => {
                    if event_data(&env).get("name").and_then(|v| v.as_str())
                        == Some("verify_passed")
                    {
                        self.record_verify(&wave_id, &task_id, "passed").await;
                    }
                }
                _ => {}
            },
            // Terminal task result per the reply contract (refocus.md §6).
            MessageKind::Message => match env.payload.get("status").and_then(|v| v.as_str()) {
                Some("done") => {
                    let result = json_text(env.payload.get("result"));
                    self.complete_task(&wave_id, &task_id, result).await;
                }
                Some("error") => {
                    let err = json_text(env.payload.get("error"));
                    self.fail_task(&wave_id, &task_id, non_empty_or(err, "task failed"))
                        .await;
                }
                Some("cancelled") => self.worker_cancelled(&wave_id, &task_id).await,
                _ => {}
            },
            _ => {}
        }
    }

    // ── Task transitions (each persists before returning) ────────────────

    async fn complete_task(&mut self, wave_id: &str, task_id: &str, result: String) {
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
    async fn fail_task(&mut self, wave_id: &str, task_id: &str, reason: String) {
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
    async fn worker_cancelled(&mut self, wave_id: &str, task_id: &str) {
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

    async fn record_verify(&mut self, wave_id: &str, task_id: &str, result: &str) {
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
    async fn cancel_unsatisfiable(&mut self, wave_id: &str) {
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

    // ── Dispatch + wave finalization ─────────────────────────────────────

    /// DM `session_start` to every pending task whose deps are all done.
    async fn dispatch_ready(&mut self, wave_id: &str) {
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
    async fn check_terminal(&mut self, wave_id: &str) {
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

    async fn finalize_wave(&mut self, wave_id: &str, status: &str, reason: Option<&str>) {
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
    async fn shutdown_wave(&mut self, wave_id: &str, status: &str, reason: &str) {
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

    // ── Liveness ─────────────────────────────────────────────────────────

    /// Record a heartbeat. On the bound `hub.presence.<identity>` subject the
    /// identity comes from the subject (pinned to the credential by NATS
    /// permissions); only the legacy subject falls back to the payload.
    fn on_presence(&mut self, subject: &str, env: &Envelope) {
        let bound = subject
            .strip_prefix(subjects::PRESENCE)
            .and_then(|rest| rest.strip_prefix('.'))
            .filter(|id| !id.is_empty());
        let ident = bound.or_else(|| env.payload.get("identity").and_then(|v| v.as_str()));
        if let Some(ident) = ident {
            self.last_seen.insert(ident.to_string(), Instant::now());
        }
    }

    /// Seed `last_seen` from the persisted agent registry so a restart
    /// doesn't instantly kill workers that were alive moments ago.
    async fn seed_liveness(&mut self) {
        let within = self.config.liveness_ttl.as_secs().max(1) as i64;
        match self
            .storage
            .find_agents(&AgentFilter::new().alive_within(within))
            .await
        {
            Ok(agents) => {
                for a in agents {
                    let inst = instant_from_datetime(a.last_seen);
                    self.last_seen
                        .entry(a.identity)
                        .and_modify(|e| {
                            if inst > *e {
                                *e = inst;
                            }
                        })
                        .or_insert(inst);
                }
            }
            Err(e) => warn!(error = %e, "failed to seed worker liveness from storage"),
        }
    }

    async fn sweep(&mut self) {
        let now = Instant::now();
        let ttl = self.config.liveness_ttl;
        let wave_ids: Vec<String> = self.waves.keys().cloned().collect();
        for wave_id in wave_ids {
            let (expired, dead) = {
                let Some(rt) = self.waves.get(&wave_id) else {
                    continue;
                };
                let dead: Vec<(String, String)> = rt
                    .tasks
                    .values()
                    .filter(|t| t.status == "running")
                    .filter(|t| {
                        let baseline = rt
                            .dispatched
                            .get(&t.task_id)
                            .copied()
                            .into_iter()
                            .chain(self.last_seen.get(&t.worker).copied())
                            .max()
                            .unwrap_or(now);
                        now.duration_since(baseline) > ttl
                    })
                    .map(|t| (t.task_id.clone(), t.worker.clone()))
                    .collect();
                (now >= rt.deadline, dead)
            };
            if expired {
                self.shutdown_wave(&wave_id, "failed", "wave timed out")
                    .await;
                continue;
            }
            for (task_id, worker) in dead {
                warn!(%wave_id, %task_id, %worker, "worker liveness TTL expired");
                self.fail_task(
                    &wave_id,
                    &task_id,
                    format!(
                        "worker '{worker}' silent for >{}s (liveness TTL)",
                        ttl.as_secs()
                    ),
                )
                .await;
            }
        }
    }

    // ── Small helpers ────────────────────────────────────────────────────

    async fn persist_task(&self, task: &WaveTaskRecord) {
        if let Err(e) = self.storage.create_wave_task(task.clone()).await {
            warn!(error = %e, wave_id = %task.wave_id, task_id = %task.task_id,
                  "failed to persist wave task");
        }
    }

    async fn publish_wave_event(&self, wave_id: &str, event_type: &str, data: Value) {
        let channel = subjects::wave_channel_name(wave_id);
        let env = Envelope::new(
            ORCHESTRATOR_IDENTITY,
            channel,
            MessageKind::Event,
            event_payload(event_type, data),
        );
        if let Err(e) = self.client.send(&env).await {
            warn!(error = %e, %wave_id, %event_type, "failed to publish wave event");
        }
    }
}

fn non_empty_or(s: String, fallback: &str) -> String {
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

fn task_to_input(t: &WaveTaskRecord) -> WaveTaskInput {
    WaveTaskInput {
        task_id: t.task_id.clone(),
        worker: t.worker.clone(),
        goal: t.goal.clone(),
        write_scope: t.write_scope.clone(),
        dependencies: t.dependencies.clone(),
        handoff_path: t.handoff_path.clone(),
        verify_cmd: t.verify_cmd.clone(),
    }
}

/// The `session_start` DM that tells a worker to run one wave task.
async fn send_session_start(
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
async fn send_cancel_dm(client: &HubClient, wave_id: &str, task: &WaveTaskRecord) {
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
