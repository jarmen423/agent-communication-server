//! API refresh — batched query-API calls for the TUI snapshot.

use anyhow::Result;
use chrono::Utc;
use serde_json::json;

use crate::query_api_client::ApiClient;
use crate::storage::{
    AgentFilter, AgentRecord, SessionFilter, SessionRecord, WaveRecord, WaveTaskRecord,
};

use super::model::Snapshot;

/// Fetch a full snapshot from the query API in parallel.
///
/// Each sub-call is independent; failures in one don't block others.
/// Missing data defaults to empty vecs so the TUI degrades gracefully.
pub async fn refresh_snapshot(api: &ApiClient) -> Result<Snapshot> {
    let (ping_res, agents_res, sessions_res, waves_res, rate_res) = tokio::join!(
        api.request("ping", json!({})),
        api.request(
            "agent.find",
            serde_json::to_value(AgentFilter::new().limit(200)).unwrap()
        ),
        api.request(
            "session.list",
            serde_json::to_value(SessionFilter::new().status("active").limit(50)).unwrap()
        ),
        api.request("wave.list", json!({})),
        api.request(
            "stats.message_rate",
            json!({"secs": 300, "interval": "minute"})
        ),
    );

    // Ping must succeed — it's our health check.
    ping_res?;

    let agents: Vec<AgentRecord> = agents_res
        .ok()
        .and_then(|v| v.get("agents").cloned())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    let sessions: Vec<SessionRecord> = sessions_res
        .ok()
        .and_then(|v| v.get("sessions").cloned())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    let waves: Vec<WaveRecord> = waves_res
        .ok()
        .and_then(|v| v.get("waves").cloned())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    let rate_points: Vec<crate::analytics::DataPoint> = rate_res
        .ok()
        .and_then(|v| v.get("data").cloned())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    Ok(Snapshot {
        agents,
        sessions,
        waves,
        rate_points,
        taken_at: Utc::now(),
    })
}

/// Fetch task counts for a single wave (lazy, on row select).
pub async fn fetch_wave_task(api: &ApiClient, wave_id: &str) -> Result<(usize, usize)> {
    let resp = api
        .request("wave.list_tasks", json!({"wave_id": wave_id}))
        .await?;
    let tasks: Vec<WaveTaskRecord> = resp
        .get("tasks")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    Ok(count_wave_tasks(&tasks))
}

/// Pure count of (done, total) for a set of wave tasks.
///
/// `done` counts only `completed` and `merged` tasks; everything else
/// (running, failed, pending, …) counts toward `total` only.
pub fn count_wave_tasks(tasks: &[WaveTaskRecord]) -> (usize, usize) {
    let total = tasks.len();
    let done = tasks
        .iter()
        .filter(|t| t.status == "completed" || t.status == "merged")
        .count();
    (done, total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn task(status: &str) -> WaveTaskRecord {
        WaveTaskRecord {
            wave_id: "w1".to_string(),
            task_id: "t".to_string(),
            worker: "worker".to_string(),
            goal: "goal".to_string(),
            status: status.to_string(),
            write_scope: vec![],
            dependencies: vec![],
            handoff_path: None,
            verify_cmd: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            result: None,
        }
    }

    #[test]
    fn test_count_wave_tasks_mixed() {
        let tasks = vec![
            task("completed"),
            task("merged"),
            task("running"),
            task("failed"),
            task("pending"),
        ];
        // done = completed + merged only; total = all
        assert_eq!(count_wave_tasks(&tasks), (2, 5));
    }

    #[test]
    fn test_count_wave_tasks_empty() {
        assert_eq!(count_wave_tasks(&[]), (0, 0));
    }

    #[test]
    fn test_count_wave_tasks_all_done() {
        let tasks = vec![task("completed"), task("merged")];
        assert_eq!(count_wave_tasks(&tasks), (2, 2));
    }
}
