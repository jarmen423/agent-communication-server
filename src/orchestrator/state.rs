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
pub const DEFAULT_TIMEOUT_SECS: u64 = 3600;
/// Shortest accepted wave timeout.
pub const MIN_TIMEOUT_SECS: u64 = 1;
/// Longest accepted wave timeout (7 days). Larger values are clamped: an
/// unclamped `u64::MAX` overflowed `Instant + Duration` and panicked the
/// orchestrator, and again on every restart when the wave was resumed.
pub const MAX_TIMEOUT_SECS: u64 = 7 * 24 * 3600;
/// `wave.metadata` key holding `{task_id: <session_start envelope id>}`, the
/// id workers register a dispatched turn under (contract C2).
pub(super) const DISPATCH_IDS_KEY: &str = "dispatch_ids";

/// Clamp a wave timeout to [`MIN_TIMEOUT_SECS`] ..= [`MAX_TIMEOUT_SECS`].
pub fn clamp_timeout_secs(secs: u64) -> u64 {
    secs.clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS)
}

/// `start + secs` (clamped), never panicking on overflow.
pub(super) fn deadline_after(start: Instant, secs: u64) -> Instant {
    let timeout = Duration::from_secs(clamp_timeout_secs(secs));
    start
        .checked_add(timeout)
        .or_else(|| Instant::now().checked_add(timeout))
        .unwrap_or_else(Instant::now)
}

/// Terminal *task* status.
pub(super) fn is_terminal(status: &str) -> bool {
    matches!(status, "done" | "failed" | "cancelled")
}

/// Terminal *wave* status (the merge-gate outcomes). A terminal wave is final.
pub(super) fn is_wave_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
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
    /// hub-server runs with `--require-bound-identity`: legacy bare
    /// `hub.presence` heartbeats (identity taken from the payload) are
    /// ignored, only `hub.presence.<identity>` counts.
    pub require_bound_identity: bool,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            liveness_ttl: Duration::from_secs(DEFAULT_LIVENESS_SECS),
            require_bound_identity: false,
        }
    }
}

/// Live (non-persisted) per-wave state. Persisted state always wins: the
/// task map is seeded from storage and every mutation is written back.
pub(super) struct WaveRt {
    /// The wave's owner (`WaveRecord::orchestrator`); owns the task sessions.
    pub(super) owner: String,
    pub(super) tasks: HashMap<String, WaveTaskRecord>,
    pub(super) done: HashSet<String>,
    pub(super) deadline: Instant,
    /// task_id → last instant its `session_start` was sent. Liveness
    /// baseline for a worker never heard from.
    pub(super) dispatched: HashMap<String, Instant>,
    /// task_id → envelope id of the latest `session_start` sent for it
    /// (contract C2: the cancel DM's `task_id`). Mirrored to
    /// `wave.metadata.dispatch_ids` so a cancel after a restart still works.
    pub(super) dispatch_ids: HashMap<String, String>,
}

impl WaveRt {
    pub(super) fn new(wave: &WaveRecord, tasks: Vec<WaveTaskRecord>, deadline: Instant) -> Self {
        let done = tasks
            .iter()
            .filter(|t| t.status == "done")
            .map(|t| t.task_id.clone())
            .collect();
        let dispatch_ids = wave
            .metadata
            .get(DISPATCH_IDS_KEY)
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            owner: wave.orchestrator.clone(),
            tasks: tasks.into_iter().map(|t| (t.task_id.clone(), t)).collect(),
            done,
            deadline,
            dispatched: HashMap::new(),
            dispatch_ids,
        }
    }

    /// Ids of tasks currently `running`.
    pub(super) fn running_ids(&self) -> Vec<String> {
        self.tasks
            .values()
            .filter(|t| t.status == "running")
            .map(|t| t.task_id.clone())
            .collect()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_is_clamped() {
        assert_eq!(clamp_timeout_secs(0), MIN_TIMEOUT_SECS);
        assert_eq!(clamp_timeout_secs(60), 60);
        assert_eq!(clamp_timeout_secs(u64::MAX), MAX_TIMEOUT_SECS);
    }

    #[test]
    fn deadline_never_overflows() {
        // Regression: `Instant::now() + Duration::from_secs(u64::MAX)` panicked.
        let now = Instant::now();
        let d = deadline_after(now, u64::MAX);
        assert_eq!(d - now, Duration::from_secs(MAX_TIMEOUT_SECS));
        assert_eq!(deadline_after(now, 5) - now, Duration::from_secs(5));
    }

    #[test]
    fn completed_wave_is_terminal() {
        for s in ["completed", "failed", "cancelled"] {
            assert!(is_wave_terminal(s), "{s}");
        }
        for s in ["pending", "running"] {
            assert!(!is_wave_terminal(s), "{s}");
        }
    }
}
