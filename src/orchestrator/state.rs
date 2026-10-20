//! Shared orchestrator state and pure helpers: constants, the live per-wave
//! runtime record, config, and the snapshot shape returned by the API.

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::Instant;

use crate::storage::{WaveRecord, WaveTaskRecord};
use crate::wave::{evaluate_merge_gate, WaveTaskInput};

/// Sender identity stamped on orchestration envelopes (task dispatches,
/// cancel DMs, wave progress events).
pub const ORCHESTRATOR_IDENTITY: &str = "hub-orchestrator";
/// Default liveness TTL: a worker silent longer loses its running tasks.
pub const DEFAULT_LIVENESS_SECS: u64 = 90;
/// Fallback wave timeout when spawn passes none and the record lacks one.
pub(super) const DEFAULT_TIMEOUT_SECS: u64 = 3600;

pub(super) fn is_terminal(status: &str) -> bool {
    matches!(status, "done" | "failed" | "cancelled")
}

/// Read a payload field as text (strings pass through; other JSON is
/// stringified) — task results are free-form per the reply contract.
pub(super) fn json_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// `wave.<id>` → (id, None); `wave.<id>.task.<tid>` → (id, Some(tid)).
pub(super) fn parse_wave_channel(channel: &str) -> Option<(String, Option<String>)> {
    let rest = channel.strip_prefix("wave.")?;
    match rest.split_once(".task.") {
        Some((wid, tid)) if !wid.is_empty() && !tid.is_empty() => {
            Some((wid.to_string(), Some(tid.to_string())))
        }
        None if !rest.contains('.') => Some((rest.to_string(), None)),
        _ => None,
    }
}

pub(super) fn instant_from_datetime(dt: DateTime<Utc>) -> Instant {
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
pub(super) struct WaveRt {
    pub(super) tasks: HashMap<String, WaveTaskRecord>,
    pub(super) done: HashSet<String>,
    pub(super) deadline: Instant,
    /// task_id → last instant its `session_start` was sent. Liveness
    /// baseline for a worker never heard from.
    pub(super) dispatched: HashMap<String, Instant>,
}

pub(super) fn non_empty_or(s: String, fallback: &str) -> String {
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

pub(super) fn task_to_input(t: &WaveTaskRecord) -> WaveTaskInput {
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
