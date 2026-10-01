//! Merge gate — derive a wave's status from its task statuses.

use crate::storage::WaveTaskRecord;

/// Evaluate the merge gate for a wave:
/// any `failed` → `failed`; any `pending`/`running` → `running`;
/// all `done` → `completed`; otherwise (all terminal, some `cancelled`) →
/// `cancelled`. An empty task list evaluates to `completed`.
pub fn evaluate_merge_gate(tasks: &[WaveTaskRecord]) -> &'static str {
    if tasks.iter().any(|t| t.status == "failed") {
        "failed"
    } else if tasks
        .iter()
        .any(|t| t.status == "pending" || t.status == "running")
    {
        "running"
    } else if tasks.iter().all(|t| t.status == "done") {
        "completed"
    } else {
        "cancelled"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn task(status: &str) -> WaveTaskRecord {
        WaveTaskRecord {
            wave_id: "w".into(),
            task_id: "t".into(),
            worker: "w".into(),
            goal: "g".into(),
            status: status.into(),
            write_scope: vec![],
            dependencies: vec![],
            handoff_path: None,
            verify_cmd: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            result: None,
            verify_result: None,
        }
    }

    #[test]
    fn gate_statuses() {
        assert_eq!(evaluate_merge_gate(&[task("done")]), "completed");
        assert_eq!(
            evaluate_merge_gate(&[task("done"), task("failed")]),
            "failed"
        );
        assert_eq!(
            evaluate_merge_gate(&[task("done"), task("pending")]),
            "running"
        );
        assert_eq!(
            evaluate_merge_gate(&[task("done"), task("cancelled")]),
            "cancelled"
        );
        assert_eq!(
            evaluate_merge_gate(&[task("cancelled"), task("cancelled")]),
            "cancelled"
        );
    }
}
