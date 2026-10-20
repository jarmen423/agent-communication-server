//! Live tests for the hub-server wave orchestrator (docs/WAVES.md).
//!
//! Each test runs a PRIVATE nats-server + hub-server pair — never the
//! with_stack-provided hub-server (its api listener is a plain subscribe,
//! so a second hub-server on the same NATS would double-answer `hub.api`
//! calls and both orchestrators would race). Needs `nats-server` (PATH or
//! `.tools/bin/`) and a built `hub-server` binary; skips otherwise.

mod wave_support;

use nats_hub::{ApiClient, HubClient, MessageKind, Storage, SurrealStorage};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use wave_support::*;

// ── Validation (no stack needed) ─────────────────────────────────

#[tokio::test]
async fn wave_create_rejects_dependency_cycle() {
    let storage: Arc<dyn Storage> = Arc::new(SurrealStorage::connect_memory().await.unwrap());
    storage.migrate().await.unwrap();
    let payload = json!({
        "op": "wave.create",
        "params": {
            "wave": {
                "wave_id": "wcycle", "goal": "g", "status": "pending",
                "orchestrator": "t", "created_at": chrono::Utc::now().to_rfc3339(),
                "metadata": {},
            },
            "tasks": [
                {"task_id": "a", "worker": "w1", "goal": "g", "write_scope": ["a"], "dependencies": ["b"]},
                {"task_id": "b", "worker": "w2", "goal": "g", "write_scope": ["b"], "dependencies": ["a"]},
            ],
        },
    });
    let resp = nats_hub::query_api::handle_request(
        &storage,
        "hub.api.wave.create",
        serde_json::to_string(&payload).unwrap().as_bytes(),
    )
    .await;
    assert!(!resp.ok, "cycle should be rejected: {resp:?}");
    assert!(
        resp.error.unwrap().contains("cycle"),
        "error should mention the cycle"
    );
    // Validation runs before anything is persisted.
    assert!(storage.get_wave("wcycle").await.unwrap().is_none());
    assert!(storage.list_wave_tasks("wcycle").await.unwrap().is_empty());
}

// ── Live orchestrator tests (private nats + hub-server) ──────────

#[tokio::test]
async fn happy_path_verify_result_and_wave_completed() {
    let _guard = STACK_LOCK.lock().unwrap();
    let Some(stack) = TestStack::new().await else {
        eprintln!("skipping: nats-server/hub-server binary not found");
        return;
    };
    let mut hub = stack.start_hub();
    stack.wait_hub().await;

    let api = ApiClient::connect(&stack.nats_url).await.unwrap();
    let ctl = HubClient::connect(&stack.nats_url, "testctl")
        .await
        .unwrap();
    let mut inbox_w1 = ctl.subscribe_channel("inbox.w1").await.unwrap();
    let mut inbox_w2 = ctl.subscribe_channel("inbox.w2").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await; // sub propagation

    let tasks = vec![
        task("t1", "w1", &[], Some("true")),
        task("t2", "w2", &["t1"], None),
    ];
    create_and_spawn(&api, "w-happy", tasks).await;

    // t1 dispatched to w1.
    let dm = recv_session_start(&mut inbox_w1, "w-happy", "t1", 30).await;
    assert_eq!(dm.meta.to.as_deref(), Some("w1"));
    assert_eq!(dm.payload["wave_id"], "w-happy");
    assert_eq!(dm.payload["channel"], "wave.w-happy.task.t1");
    assert_eq!(dm.payload["task_channel"], "wave.w-happy.task.t1");
    assert_eq!(dm.payload["session_id"], "w-happy--t1");
    assert_eq!(dm.payload["verify_cmd"], "true");

    // The wave task has a session record, so the worker's resume hook
    // (session.set_backend_ctx) works for it.
    let sess = api
        .request("session.get", json!({"session_id": "w-happy--t1"}))
        .await
        .unwrap();
    assert_eq!(sess["session"]["worker"], "w1");
    // Owned by the identity that sends the session DMs (workers check it).
    assert_eq!(sess["session"]["orchestrator"], "hub-orchestrator");
    assert_eq!(sess["session"]["metadata"]["wave_owner"], "test");
    assert_eq!(sess["session"]["status"], "active");
    api.request(
        "session.set_backend_ctx",
        json!({"session_id": "w-happy--t1", "backend_ctx": {"claude_session_id": "abc"}}),
    )
    .await
    .expect("set_backend_ctx on a wave task session");

    // w1 works: verify milestone, then completed.
    let w1 = HubClient::connect(&stack.nats_url, "w1").await.unwrap();
    emit_task_event(
        &w1,
        "w-happy",
        "t1",
        "milestone",
        json!({"name": "verify_passed"}),
    )
    .await;
    emit_task_event(
        &w1,
        "w-happy",
        "t1",
        "completed",
        json!({"result": "t1 done"}),
    )
    .await;

    // t2 is dispatched to w2 only after t1 finishes (dependency).
    recv_session_start(&mut inbox_w2, "w-happy", "t2", 30).await;

    let w2 = HubClient::connect(&stack.nats_url, "w2").await.unwrap();
    emit_task_event(
        &w2,
        "w-happy",
        "t2",
        "completed",
        json!({"result": "t2 done"}),
    )
    .await;

    let snap = wait_status(&api, "w-happy", 30, |s| s["wave"]["status"] == "completed").await;
    assert_eq!(task_status(&snap, "t1"), "done");
    assert_eq!(task_status(&snap, "t2"), "done");
    assert_eq!(task_field(&snap, "t1", "verify_result"), "passed");
    assert_eq!(task_field(&snap, "t1", "result"), "t1 done");
    assert_eq!(snap["summary"]["merge_gate"], "completed");

    // C4: each finished task's session is closed (DM + record).
    recv_action(&mut inbox_w1, "session_close", "w-happy", "t1", 5).await;
    let sess = api
        .request("session.get", json!({"session_id": "w-happy--t1"}))
        .await
        .unwrap();
    assert_eq!(sess["session"]["status"], "closed");
    assert_eq!(sess["session"]["backend_ctx"]["claude_session_id"], "abc");

    // A completed wave is final: wave.cancel must not flip it.
    assert!(api
        .request("wave.cancel", json!({"wave_id": "w-happy"}))
        .await
        .is_err());
    let snap = wave_status(&api, "w-happy").await;
    assert_eq!(snap["wave"]["status"], "completed");
    assert_eq!(task_status(&snap, "t1"), "done");

    hub.kill().await.unwrap();
}

#[tokio::test]
async fn foreign_sender_cannot_complete_task() {
    let _guard = STACK_LOCK.lock().unwrap();
    let Some(stack) = TestStack::new().await else {
        eprintln!("skipping: nats-server/hub-server binary not found");
        return;
    };
    let mut hub = stack.start_hub();
    stack.wait_hub().await;

    let api = ApiClient::connect(&stack.nats_url).await.unwrap();
    let ctl = HubClient::connect(&stack.nats_url, "testctl")
        .await
        .unwrap();
    let mut inbox_w1 = ctl.subscribe_channel("inbox.w1").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    create_and_spawn(&api, "w-foreign", vec![task("t1", "w1", &[], None)]).await;
    recv_session_start(&mut inbox_w1, "w-foreign", "t1", 30).await;

    // An impostor tries to mark t1 completed.
    let mallory = HubClient::connect(&stack.nats_url, "mallory")
        .await
        .unwrap();
    emit_task_event(
        &mallory,
        "w-foreign",
        "t1",
        "completed",
        json!({"result": "forged"}),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let snap = wave_status(&api, "w-foreign").await;
    assert_eq!(
        task_status(&snap, "t1"),
        "running",
        "foreign completion must be ignored"
    );
    assert_eq!(snap["wave"]["status"], "running");

    // The real worker's completion lands.
    let w1 = HubClient::connect(&stack.nats_url, "w1").await.unwrap();
    emit_task_event(
        &w1,
        "w-foreign",
        "t1",
        "completed",
        json!({"result": "legit"}),
    )
    .await;
    let snap = wait_status(&api, "w-foreign", 30, |s| {
        s["wave"]["status"] == "completed"
    })
    .await;
    assert_eq!(task_field(&snap, "t1", "result"), "legit");

    hub.kill().await.unwrap();
}

#[tokio::test]
async fn dead_worker_fails_wave_and_cancels_rest() {
    let _guard = STACK_LOCK.lock().unwrap();
    let Some(stack) = TestStack::new().await else {
        eprintln!("skipping: nats-server/hub-server binary not found");
        return;
    };
    let mut hub = stack.start_hub();
    stack.wait_hub().await;

    let api = ApiClient::connect(&stack.nats_url).await.unwrap();
    let ctl = HubClient::connect(&stack.nats_url, "testctl")
        .await
        .unwrap();
    let mut inbox_w1 = ctl.subscribe_channel("inbox.w1").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // w1 is assigned t1 but never connects — it is a ghost.
    create_and_spawn(
        &api,
        "w-dead",
        vec![task("t1", "w1", &[], None), task("t2", "w2", &["t1"], None)],
    )
    .await;
    recv_session_start(&mut inbox_w1, "w-dead", "t1", 30).await;

    // Liveness TTL (3s) + sweep (~1s): t1 must fail, wave must fail,
    // t2 (never dispatched) must be cancelled — fail-fast.
    let snap = wait_status(&api, "w-dead", 30, |s| s["wave"]["status"] == "failed").await;
    assert_eq!(task_status(&snap, "t1"), "failed");
    assert_eq!(task_status(&snap, "t2"), "cancelled");
    assert!(
        task_field(&snap, "t1", "result")
            .as_str()
            .unwrap_or("")
            .contains("liveness"),
        "task result should explain the liveness failure"
    );
    assert_eq!(snap["summary"]["merge_gate"], "failed");

    hub.kill().await.unwrap();
}

#[tokio::test]
async fn worker_error_is_fail_fast() {
    let _guard = STACK_LOCK.lock().unwrap();
    let Some(stack) = TestStack::new().await else {
        eprintln!("skipping: nats-server/hub-server binary not found");
        return;
    };
    let mut hub = stack.start_hub();
    stack.wait_hub().await;

    let api = ApiClient::connect(&stack.nats_url).await.unwrap();
    let ctl = HubClient::connect(&stack.nats_url, "testctl")
        .await
        .unwrap();
    let mut inbox_w1 = ctl.subscribe_channel("inbox.w1").await.unwrap();
    let mut inbox_w2 = ctl.subscribe_channel("inbox.w2").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    create_and_spawn(
        &api,
        "w-fail",
        vec![
            task("t1", "w1", &[], Some("make test")),
            task("t2", "w2", &["t1"], None),
        ],
    )
    .await;
    recv_session_start(&mut inbox_w1, "w-fail", "t1", 30).await;

    let w1 = HubClient::connect(&stack.nats_url, "w1").await.unwrap();
    emit_task_event(&w1, "w-fail", "t1", "error", json!({"error": "boom"})).await;

    let snap = wait_status(&api, "w-fail", 30, |s| s["wave"]["status"] == "failed").await;
    assert_eq!(task_status(&snap, "t1"), "failed");
    assert_eq!(task_status(&snap, "t2"), "cancelled");
    assert_eq!(task_field(&snap, "t1", "verify_result"), "failed");
    assert_eq!(task_field(&snap, "t1", "result"), "boom");
    // t2 never got dispatched.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), inbox_w2.recv())
            .await
            .is_err()
    );

    hub.kill().await.unwrap();
}

#[tokio::test]
async fn wave_survives_hub_server_restart() {
    let _guard = STACK_LOCK.lock().unwrap();
    let Some(stack) = TestStack::new().await else {
        eprintln!("skipping: nats-server/hub-server binary not found");
        return;
    };
    let mut hub = stack.start_hub();
    stack.wait_hub().await;

    let api = ApiClient::connect(&stack.nats_url).await.unwrap();
    let ctl = HubClient::connect(&stack.nats_url, "testctl")
        .await
        .unwrap();
    let mut inbox_w1 = ctl.subscribe_channel("inbox.w1").await.unwrap();
    let mut inbox_w2 = ctl.subscribe_channel("inbox.w2").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Long-timeout wave so the restart window is comfortable.
    create_and_spawn(
        &api,
        "w-restart",
        vec![task("t1", "w1", &[], None), task("t2", "w2", &["t1"], None)],
    )
    .await;
    let first = recv_session_start(&mut inbox_w1, "w-restart", "t1", 30).await;

    // Kill hub-server mid-wave; the nats-server and the wave's persisted
    // state survive. Restart on the same DB.
    hub.kill().await.unwrap();
    hub.wait().await.unwrap();
    let mut hub = stack.start_hub();
    stack.wait_hub().await;

    // The resumed orchestrator re-dispatches the still-running task
    // (at-least-once): a second session_start lands for t1, with the same
    // session id (C3 dedupes it on the worker) and a new envelope id, which
    // is now the task's cancel id (C2, persisted in wave metadata).
    let again = recv_session_start(&mut inbox_w1, "w-restart", "t1", 15).await;
    assert_eq!(again.payload["session_id"], first.payload["session_id"]);
    assert_ne!(again.meta.id, first.meta.id);
    // (Persisted right after the send; poll so the read can't race it.)
    wait_status(&api, "w-restart", 10, |s| {
        s["wave"]["metadata"]["dispatch_ids"]["t1"] == again.meta.id.as_str()
    })
    .await;

    // And the wave still drives forward: w1 completes → t2 dispatches.
    let w1 = HubClient::connect(&stack.nats_url, "w1").await.unwrap();
    emit_task_event(&w1, "w-restart", "t1", "completed", json!({"result": "ok"})).await;
    recv_session_start(&mut inbox_w2, "w-restart", "t2", 30).await;
    let w2 = HubClient::connect(&stack.nats_url, "w2").await.unwrap();
    emit_task_event(&w2, "w-restart", "t2", "completed", json!({"result": "ok"})).await;

    let snap = wait_status(&api, "w-restart", 30, |s| {
        s["wave"]["status"] == "completed"
    })
    .await;
    assert_eq!(snap["summary"]["merge_gate"], "completed");

    hub.kill().await.unwrap();
}

#[tokio::test]
async fn wave_cancel_marks_tasks_and_dms_workers() {
    let _guard = STACK_LOCK.lock().unwrap();
    let Some(stack) = TestStack::new().await else {
        eprintln!("skipping: nats-server/hub-server binary not found");
        return;
    };
    let mut hub = stack.start_hub();
    stack.wait_hub().await;

    let api = ApiClient::connect(&stack.nats_url).await.unwrap();
    let ctl = HubClient::connect(&stack.nats_url, "testctl")
        .await
        .unwrap();
    let mut inbox_w1 = ctl.subscribe_channel("inbox.w1").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    create_and_spawn(
        &api,
        "w-cancel",
        vec![task("t1", "w1", &[], None), task("t2", "w2", &["t1"], None)],
    )
    .await;
    let start = recv_session_start(&mut inbox_w1, "w-cancel", "t1", 30).await;

    api.request("wave.cancel", json!({"wave_id": "w-cancel"}))
        .await
        .expect("wave.cancel");

    let snap = wave_status(&api, "w-cancel").await;
    assert_eq!(snap["wave"]["status"], "cancelled");
    assert_eq!(task_status(&snap, "t1"), "cancelled");
    assert_eq!(task_status(&snap, "t2"), "cancelled");

    // §4.2 + C2: the running task's worker gets a kind=control cancel DM
    // whose task_id is the session_start envelope id (what workers register
    // the turn under), from the identity that dispatched it (C5).
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_cancel = false;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), inbox_w1.recv()).await {
            Ok(Some(env)) => {
                if env.meta.kind == MessageKind::Control && env.payload["action"] == "cancel" {
                    assert_eq!(env.payload["task_id"], start.meta.id.as_str());
                    assert_eq!(env.payload["wave_task_id"], "t1");
                    assert_eq!(env.meta.from, start.meta.from);
                    saw_cancel = true;
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(saw_cancel, "worker never received the §4.2 cancel DM");
    // C4: followed by session_close for the wave task's session.
    recv_action(&mut inbox_w1, "session_close", "w-cancel", "t1", 5).await;

    // Terminal states are final: cancelling again fails, status unchanged.
    assert!(api
        .request("wave.cancel", json!({"wave_id": "w-cancel"}))
        .await
        .is_err());

    hub.kill().await.unwrap();
}

/// Regression (iteration-2 integration): since T1, workers heartbeat on the
/// bound `hub.presence.<identity>` subject. The orchestrator must count those,
/// or a live worker running a long task without wave events is failed as
/// "silent" once the liveness TTL passes.
#[tokio::test]
async fn bound_heartbeats_keep_long_running_task_alive() {
    let _guard = STACK_LOCK.lock().unwrap();
    let Some(stack) = TestStack::new().await else {
        eprintln!("skipping: nats-server/hub-server binary not found");
        return;
    };
    let mut hub = stack.start_hub();
    stack.wait_hub().await;

    let api = ApiClient::connect(&stack.nats_url).await.unwrap();
    let w1 = HubClient::connect(&stack.nats_url, "w1").await.unwrap();
    let mut inbox_w1 = w1.subscribe_inbox().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    create_and_spawn(&api, "w-alive", vec![task("t1", "w1", &[], None)]).await;
    recv_session_start(&mut inbox_w1, "w-alive", "t1", 30).await;

    // Work "silently" for well past the 3s TTL, heartbeating only (bound subject).
    for _ in 0..8 {
        w1.heartbeat().await.unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let snap = wave_status(&api, "w-alive").await;
    assert_eq!(
        task_status(&snap, "t1"),
        "running",
        "a heartbeating worker must not be failed for silence: {snap}"
    );

    emit_task_event(&w1, "w-alive", "t1", "completed", json!({"result": "ok"})).await;
    wait_status(&api, "w-alive", 30, |s| s["wave"]["status"] == "completed").await;

    hub.kill().await.unwrap();
}
