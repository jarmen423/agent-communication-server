//! Live tests for the hub-server wave orchestrator (docs/WAVES.md).
//!
//! Each test runs a PRIVATE nats-server + hub-server pair — never the
//! with_stack-provided hub-server (its api listener is a plain subscribe,
//! so a second hub-server on the same NATS would double-answer `hub.api`
//! calls and both orchestrators would race). Needs `nats-server` (PATH or
//! `.tools/bin/`) and a built `hub-server` binary; skips otherwise.

use nats_hub::protocol::subjects;
use nats_hub::{ApiClient, Envelope, HubClient, MessageKind, Storage, SurrealStorage};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::process::{Child, Command};

const LIVENESS_SECS: &str = "3";

/// Live tests each boot a private nats-server + hub-server (each with an
/// embedded SurrealDB); serialize them so parallel stacks don't starve
/// each other during startup.
static STACK_LOCK: Mutex<()> = Mutex::new(());

fn nats_server_bin() -> Option<PathBuf> {
    if let Ok(out) = std::process::Command::new("nats-server")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        if out.success() {
            return Some(PathBuf::from("nats-server"));
        }
    }
    let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tools/bin/nats-server");
    local.exists().then_some(local)
}

fn hub_server_bin() -> Option<PathBuf> {
    if let Some(p) = option_env!("CARGO_BIN_EXE_hub-server") {
        return Some(PathBuf::from(p));
    }
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let p = target.join("debug/hub-server");
    p.exists().then_some(p)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A private nats-server + a (re)startable hub-server on its own DB.
struct TestStack {
    _tmp: TempDir,
    nats: Child,
    db_dir: PathBuf,
    nats_url: String,
}

impl TestStack {
    async fn new() -> Option<Self> {
        let (nats_bin, hub_bin) = (nats_server_bin()?, hub_server_bin()?);
        let _ = hub_bin;
        let tmp = TempDir::new().ok()?;
        let port = free_port();
        let js = tmp.path().join("js");
        let nats = Command::new(nats_bin)
            .args([
                "-a",
                "127.0.0.1",
                "-p",
                &port.to_string(),
                "-js",
                "-sd",
                js.to_str()?,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let nats_url = format!("nats://127.0.0.1:{port}");
        // Wait for the port to accept connections.
        for _ in 0..50 {
            if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Some(Self {
            db_dir: tmp.path().join("db"),
            nats,
            _tmp: tmp,
            nats_url,
        })
    }

    fn start_hub(&self) -> Child {
        Command::new(hub_server_bin().unwrap())
            .args([
                "--nats-url",
                &self.nats_url,
                "--db-path",
                self.db_dir.to_str().unwrap(),
                "--wave-liveness-secs",
                LIVENESS_SECS,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("hub-server spawn")
    }

    /// Poll until the query API answers (hub-server fully up).
    async fn wait_hub(&self) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if let Ok(api) = ApiClient::connect(&self.nats_url).await {
                if api.request("wave.list", json!({})).await.is_ok() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("hub-server query API never came up on {}", self.nats_url);
    }
}

impl Drop for TestStack {
    fn drop(&mut self) {
        let _ = self.nats.start_kill();
    }
}

/// Create a wave + tasks through the API and spawn it.
async fn create_and_spawn(api: &ApiClient, wave_id: &str, tasks: Vec<Value>) {
    let created = api
        .request(
            "wave.create",
            json!({
                "wave": {
                    "wave_id": wave_id,
                    "goal": "test wave",
                    "status": "pending",
                    "orchestrator": "test",
                    "created_at": chrono::Utc::now().to_rfc3339(),
                    "metadata": {},
                },
                "tasks": tasks,
            }),
        )
        .await
        .expect("wave.create");
    assert_eq!(created["wave_id"].as_str().unwrap(), wave_id);
    api.request(
        "wave.spawn",
        json!({"wave_id": wave_id, "timeout_secs": 60}),
    )
    .await
    .expect("wave.spawn");
}

fn task(task_id: &str, worker: &str, deps: &[&str], verify_cmd: Option<&str>) -> Value {
    json!({
        "task_id": task_id,
        "worker": worker,
        "goal": format!("goal {task_id}"),
        "write_scope": [format!("src/{task_id}")],
        "dependencies": deps,
        "verify_cmd": verify_cmd,
    })
}

async fn wave_status(api: &ApiClient, wave_id: &str) -> Value {
    api.request("wave.status", json!({"wave_id": wave_id}))
        .await
        .expect("wave.status")
}

/// Poll `wave.status` until `pred` holds (or panic at `secs`).
async fn wait_status<F>(api: &ApiClient, wave_id: &str, secs: u64, pred: F) -> Value
where
    F: Fn(&Value) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let snap = wave_status(api, wave_id).await;
        if pred(&snap) {
            return snap;
        }
        assert!(
            Instant::now() < deadline,
            "wave.status predicate timed out: {snap}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

fn task_status(snap: &Value, task_id: &str) -> String {
    snap["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task_id"] == task_id)
        .map(|t| t["status"].as_str().unwrap_or("?").to_string())
        .unwrap_or_else(|| "missing".into())
}

fn task_field<'a>(snap: &'a Value, task_id: &str, field: &str) -> &'a Value {
    &snap["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task_id"] == task_id)
        .unwrap()[field]
}

/// Wait for a `session_start` DM for `task_id` on a worker inbox receiver.
async fn recv_session_start(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Envelope>,
    task_id: &str,
    secs: u64,
) -> Envelope {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while let Ok(Some(env)) = tokio::time::timeout(
        deadline.saturating_duration_since(Instant::now()),
        rx.recv(),
    )
    .await
    {
        if env.payload.get("action").and_then(|v| v.as_str()) == Some("session_start")
            && env.payload.get("session_id").and_then(|v| v.as_str()) == Some(task_id)
        {
            return env;
        }
    }
    panic!("no session_start for {task_id} within {secs}s");
}

/// Publish a worker event on a task channel (`event_type`, `data`).
async fn emit_task_event(client: &HubClient, wave_id: &str, task_id: &str, ty: &str, data: Value) {
    let channel = subjects::wave_task_channel_name(wave_id, task_id);
    let env = Envelope::new(
        client.identity(),
        channel,
        MessageKind::Event,
        json!({"event_type": ty, "data": data}),
    );
    client.send(&env).await.unwrap();
}

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
    let dm = recv_session_start(&mut inbox_w1, "t1", 30).await;
    assert_eq!(dm.meta.to.as_deref(), Some("w1"));
    assert_eq!(dm.payload["wave_id"], "w-happy");
    assert_eq!(dm.payload["channel"], "wave.w-happy.task.t1");
    assert_eq!(dm.payload["verify_cmd"], "true");

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
    recv_session_start(&mut inbox_w2, "t2", 30).await;

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
    recv_session_start(&mut inbox_w1, "t1", 30).await;

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
    recv_session_start(&mut inbox_w1, "t1", 30).await;

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
    recv_session_start(&mut inbox_w1, "t1", 30).await;

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
    recv_session_start(&mut inbox_w1, "t1", 30).await;

    // Kill hub-server mid-wave; the nats-server and the wave's persisted
    // state survive. Restart on the same DB.
    hub.kill().await.unwrap();
    hub.wait().await.unwrap();
    let mut hub = stack.start_hub();
    stack.wait_hub().await;

    // The resumed orchestrator re-dispatches the still-running task
    // (at-least-once): a second session_start lands for t1.
    recv_session_start(&mut inbox_w1, "t1", 15).await;

    // And the wave still drives forward: w1 completes → t2 dispatches.
    let w1 = HubClient::connect(&stack.nats_url, "w1").await.unwrap();
    emit_task_event(&w1, "w-restart", "t1", "completed", json!({"result": "ok"})).await;
    recv_session_start(&mut inbox_w2, "t2", 30).await;
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
    recv_session_start(&mut inbox_w1, "t1", 30).await;

    api.request("wave.cancel", json!({"wave_id": "w-cancel"}))
        .await
        .expect("wave.cancel");

    let snap = wave_status(&api, "w-cancel").await;
    assert_eq!(snap["wave"]["status"], "cancelled");
    assert_eq!(task_status(&snap, "t1"), "cancelled");
    assert_eq!(task_status(&snap, "t2"), "cancelled");

    // §4.2: the running task's worker gets a kind=control cancel DM.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_cancel = false;
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), inbox_w1.recv()).await {
            Ok(Some(env)) => {
                if env.meta.kind == MessageKind::Control
                    && env.payload["action"] == "cancel"
                    && env.payload["task_id"] == "t1"
                {
                    saw_cancel = true;
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(saw_cancel, "worker never received the §4.2 cancel DM");

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
    recv_session_start(&mut inbox_w1, "t1", 30).await;

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
