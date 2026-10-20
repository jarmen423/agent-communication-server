//! Integration tests for wave storage and validation.

use chrono::Utc;
use nats_hub::wave::validate_tasks;
use nats_hub::{Storage, SurrealStorage, WaveRecord, WaveTaskInput, WaveTaskRecord};
use serde_json::json;

async fn setup() -> SurrealStorage {
    let storage = SurrealStorage::connect_memory().await.unwrap();
    storage.migrate().await.unwrap();
    storage
}

fn make_wave(id: &str, goal: &str) -> WaveRecord {
    WaveRecord {
        wave_id: id.to_string(),
        goal: goal.to_string(),
        status: "pending".to_string(),
        orchestrator: "orch".to_string(),
        created_at: Utc::now(),
        closed_at: None,
        metadata: json!({}),
    }
}

fn make_task(wave_id: &str, task_id: &str, worker: &str) -> WaveTaskRecord {
    WaveTaskRecord {
        wave_id: wave_id.to_string(),
        task_id: task_id.to_string(),
        worker: worker.to_string(),
        goal: format!("goal for {task_id}"),
        status: "pending".to_string(),
        write_scope: vec![format!("src/{task_id}")],
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

#[tokio::test]
async fn test_wave_create_and_status() {
    let storage = setup().await;

    storage
        .create_wave(make_wave("wave-001", "test goal"))
        .await
        .unwrap();

    let wave = storage.get_wave("wave-001").await.unwrap();
    assert!(wave.is_some());
    assert_eq!(wave.unwrap().status, "pending");

    storage
        .update_wave_status("wave-001", "running")
        .await
        .unwrap();
    storage
        .update_wave_status("wave-001", "completed")
        .await
        .unwrap();

    let wave = storage.get_wave("wave-001").await.unwrap().unwrap();
    assert_eq!(wave.status, "completed");
    assert!(wave.closed_at.is_some());
}

#[tokio::test]
async fn test_wave_task_lifecycle() {
    let storage = setup().await;

    storage
        .create_wave(make_wave("wave-002", "parallel work"))
        .await
        .unwrap();
    storage
        .create_wave_task(make_task("wave-002", "task-a", "worker-1"))
        .await
        .unwrap();
    storage
        .create_wave_task(make_task("wave-002", "task-b", "worker-2"))
        .await
        .unwrap();

    storage
        .update_wave_task_status("wave-002", "task-a", "running", None)
        .await
        .unwrap();
    storage
        .update_wave_task_status("wave-002", "task-a", "done", Some("ok"))
        .await
        .unwrap();

    let task = storage
        .get_wave_task("wave-002", "task-a")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task.status, "done");
    assert_eq!(task.result.as_deref(), Some("ok"));
    assert!(task.completed_at.is_some());

    let tasks = storage.list_wave_tasks("wave-002").await.unwrap();
    assert_eq!(tasks.len(), 2);
}

#[tokio::test]
async fn test_wave_list_by_status() {
    let storage = setup().await;

    storage
        .create_wave(make_wave("wave-a", "one"))
        .await
        .unwrap();
    storage
        .create_wave(make_wave("wave-b", "two"))
        .await
        .unwrap();
    storage
        .update_wave_status("wave-a", "completed")
        .await
        .unwrap();

    let completed = storage.list_waves(Some("completed")).await.unwrap();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].wave_id, "wave-a");

    let all = storage.list_waves(None).await.unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn test_validate_tasks_rejects_overlap() {
    let tasks = vec![
        WaveTaskInput {
            task_id: "a".to_string(),
            worker: "w1".to_string(),
            goal: "g".to_string(),
            write_scope: vec!["src".to_string()],
            dependencies: vec![],
            handoff_path: None,
            verify_cmd: None,
        },
        WaveTaskInput {
            task_id: "b".to_string(),
            worker: "w2".to_string(),
            goal: "g".to_string(),
            write_scope: vec!["src/lib".to_string()],
            dependencies: vec![],
            handoff_path: None,
            verify_cmd: None,
        },
    ];
    assert!(validate_tasks(&tasks).is_err());
}

#[test]
fn test_evaluate_merge_gate() {
    use nats_hub::evaluate_merge_gate;

    let done = vec![WaveTaskRecord {
        wave_id: "w".into(),
        task_id: "t1".into(),
        worker: "w".into(),
        goal: "g".into(),
        status: "done".into(),
        write_scope: vec![],
        dependencies: vec![],
        handoff_path: None,
        verify_cmd: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        result: None,
        verify_result: None,
    }];
    assert_eq!(evaluate_merge_gate(&done), "completed");

    let failed = vec![WaveTaskRecord {
        status: "failed".into(),
        ..done[0].clone()
    }];
    assert_eq!(evaluate_merge_gate(&failed), "failed");
}

// ── H2 hardening: storage + API state rules (no NATS) ─────────────────

#[tokio::test]
async fn wave_metadata_key_is_a_field_update() {
    let storage = setup().await;
    let mut wave = make_wave("w-meta", "g");
    wave.metadata = json!({"timeout_secs": 60});
    storage.create_wave(wave).await.unwrap();
    storage
        .update_wave_status("w-meta", "running")
        .await
        .unwrap();

    let found = storage
        .set_wave_metadata_key("w-meta", "dispatch_ids", json!({"t1": "env-1"}))
        .await
        .unwrap();
    assert!(found);
    let w = storage.get_wave("w-meta").await.unwrap().unwrap();
    assert_eq!(w.status, "running", "metadata write must not touch status");
    assert_eq!(w.metadata["timeout_secs"], 60);
    assert_eq!(w.metadata["dispatch_ids"]["t1"], "env-1");

    assert!(!storage
        .set_wave_metadata_key("no-such-wave", "dispatch_ids", json!({}))
        .await
        .unwrap());
    assert!(storage
        .set_wave_metadata_key("w-meta", "bad key; DELETE waves", json!(1))
        .await
        .is_err());
}

async fn api(
    s: &std::sync::Arc<dyn Storage>,
    op: &str,
    params: serde_json::Value,
) -> nats_hub::query_api::ApiResponse {
    let body = serde_json::to_vec(&json!({"op": op, "params": params})).unwrap();
    nats_hub::query_api::handle_request(s, &format!("hub.api.{op}"), &body).await
}

#[tokio::test]
async fn terminal_states_are_final_via_api() {
    let s: std::sync::Arc<dyn Storage> = std::sync::Arc::new(setup().await);
    let r = api(
        &s,
        "wave.create",
        json!({
            "wave": {"wave_id": "w-final", "goal": "g", "status": "pending",
                     "orchestrator": "o", "created_at": Utc::now().to_rfc3339()},
            "tasks": [{"task_id": "t1", "worker": "w1", "goal": "g"}],
        }),
    )
    .await;
    assert!(r.ok, "{:?}", r.error);

    // A done task can't be flipped to anything else; re-setting done is a no-op.
    let set = |status: &str| json!({"wave_id": "w-final", "task_id": "t1", "status": status});
    assert!(api(&s, "wave.update_task_status", set("done")).await.ok);
    assert!(api(&s, "wave.update_task_status", set("done")).await.ok);
    let r = api(&s, "wave.update_task_status", set("pending")).await;
    assert!(!r.ok && r.error.unwrap().contains("final"));

    // A completed wave can't be cancelled or re-statused or re-created.
    s.update_wave_status("w-final", "completed").await.unwrap();
    let r = api(
        &s,
        "wave.update_status",
        json!({"wave_id": "w-final", "status": "cancelled"}),
    )
    .await;
    assert!(!r.ok && r.error.unwrap().contains("final"));
    let r = api(
        &s,
        "wave.create",
        json!({"wave": {"wave_id": "w-final", "goal": "g", "status": "pending",
                        "orchestrator": "o", "created_at": Utc::now().to_rfc3339()}}),
    )
    .await;
    assert!(!r.ok, "re-creating a completed wave must fail");
    assert_eq!(
        s.get_wave("w-final").await.unwrap().unwrap().status,
        "completed"
    );
}

#[tokio::test]
async fn wave_create_rejects_ids_that_are_not_one_token() {
    let s: std::sync::Arc<dyn Storage> = std::sync::Arc::new(setup().await);
    for (wave_id, task_id) in [("w.1", "t1"), ("w--1", "t1"), ("w1", "t.1"), ("w1", "a--b")] {
        let r = api(
            &s,
            "wave.create",
            json!({
                "wave": {"wave_id": wave_id, "goal": "g", "status": "pending",
                         "orchestrator": "o", "created_at": Utc::now().to_rfc3339()},
                "tasks": [{"task_id": task_id, "worker": "w1", "goal": "g"}],
            }),
        )
        .await;
        assert!(!r.ok, "{wave_id}/{task_id} should be rejected");
    }
}
