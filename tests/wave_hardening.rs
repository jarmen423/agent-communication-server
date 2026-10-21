//! Live regression tests for the 2026-10-20 wave orchestrator hardening (H2,
//! `.planning/refocus-2/HARDENING.md`): timeout clamping, cancel reaching
//! the worker's turn (C2), the Rust `hub-worker` completing a wave task
//! (C6), completions for tasks that aren't running, API status updates
//! going through the orchestrator, and legacy-presence spoofing.
//!
//! Like `wave_orchestrator.rs`, each test boots a PRIVATE nats-server +
//! hub-server and skips when the binaries are missing.

#![allow(clippy::await_holding_lock)] // STACK_LOCK serializes whole live tests on purpose

mod support;

mod wave_support;

use futures_util::StreamExt;
use nats_hub::{ApiClient, HubClient, Storage, SurrealStorage, WaveRecord, WaveTaskRecord};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use wave_support::*;

const WEEK_SECS: u64 = 7 * 24 * 3600;

/// Wait until `identity` registers (bound `hub.register.<identity>`).
async fn wait_registered(reg: &mut async_nats::Subscriber, identity: &str, child: &mut Child) {
    let subject = format!("hub.register.{identity}");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("{identity} exited before registering: {status}");
        }
        assert!(Instant::now() < deadline, "{identity} never registered");
        if let Ok(Some(msg)) = tokio::time::timeout(Duration::from_millis(500), reg.next()).await {
            if msg.subject.as_str() == subject {
                return;
            }
        }
    }
}

fn hub_worker_bin() -> PathBuf {
    PathBuf::from(support::client_bin("hub-worker"))
}

fn spawn_hub_worker(stack: &TestStack, identity: &str, execute: &str) -> Child {
    Command::new(hub_worker_bin())
        .args([
            "--identity",
            identity,
            "--execute",
            execute,
            "--nats-url",
            &stack.nats_url,
            "--heartbeat-secs",
            "1",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("hub-worker spawn")
}

/// True if `pid` is a live, non-zombie process (Linux `/proc`).
fn pid_alive(pid: u64) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z" && state != "X"),
        Err(_) => false,
    }
}

// ── Item 1: timeout clamping ──────────────────────────────────────

#[tokio::test]
async fn spawn_with_huge_timeout_is_clamped_not_a_panic() {
    let _guard = STACK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(stack) = TestStack::new().await else {
        eprintln!("skipping: nats-server/hub-server binary not found");
        return;
    };
    let mut hub = stack.start_hub();
    stack.wait_hub().await;
    let api = ApiClient::connect(&stack.nats_url).await.unwrap();

    create_wave(&api, "w-huge", vec![task("t1", "w1", &[], None)]).await;
    // A negative / non-integer timeout is rejected at the API boundary.
    assert!(api
        .request(
            "wave.spawn",
            json!({"wave_id": "w-huge", "timeout_secs": -5})
        )
        .await
        .is_err());
    // u64::MAX used to overflow `Instant + Duration` and panic the orchestrator.
    let snap = api
        .request(
            "wave.spawn",
            json!({"wave_id": "w-huge", "timeout_secs": u64::MAX}),
        )
        .await
        .expect("spawn with u64::MAX timeout");
    assert_eq!(snap["wave"]["status"], "running");
    assert_eq!(snap["wave"]["metadata"]["timeout_secs"], WEEK_SECS);

    // The orchestrator is still alive: a second wave spawns fine.
    create_and_spawn(&api, "w-after", vec![task("t1", "w2", &[], None)]).await;
    assert_eq!(
        wave_status(&api, "w-after").await["wave"]["status"],
        "running"
    );
    assert!(hub.try_wait().unwrap().is_none(), "hub-server died");
    hub.kill().await.unwrap();
}

#[tokio::test]
async fn resume_clamps_a_stored_out_of_range_timeout() {
    let _guard = STACK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(stack) = TestStack::new().await else {
        eprintln!("skipping: nats-server/hub-server binary not found");
        return;
    };
    // Seed a running wave whose stored timeout would overflow the deadline
    // (written before the API clamped it), straight into the DB — from a
    // child process, so the RocksDB lock is released when it exits.
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "seed_stored_timeout_db",
            "--ignored",
            "--test-threads",
            "1",
        ])
        .env(SEED_DB_ENV, &stack.db_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "seeding the DB failed");

    let ctl = HubClient::connect(&stack.nats_url, "testctl")
        .await
        .unwrap();
    let mut inbox_w1 = ctl.subscribe_channel("inbox.w1").await.unwrap();
    let mut hub = stack.start_hub();
    stack.wait_hub().await;

    // Resume re-dispatches the running task instead of panicking.
    recv_session_start(&mut inbox_w1, "w-stored", "t1", 30).await;
    assert!(hub.try_wait().unwrap().is_none(), "hub-server died");
    let w1 = HubClient::connect(&stack.nats_url, "w1").await.unwrap();
    emit_task_event(&w1, "w-stored", "t1", "completed", json!({"result": "ok"})).await;
    wait_status(
        &ApiClient::connect(&stack.nats_url).await.unwrap(),
        "w-stored",
        30,
        |s| s["wave"]["status"] == "completed",
    )
    .await;
    hub.kill().await.unwrap();
}

const SEED_DB_ENV: &str = "NATS_HUB_TEST_SEED_DB";

/// Helper run in a child process by `resume_clamps_a_stored_out_of_range_timeout`.
#[tokio::test]
#[ignore = "helper: run by resume_clamps_a_stored_out_of_range_timeout"]
async fn seed_stored_timeout_db() {
    let Ok(db) = std::env::var(SEED_DB_ENV) else {
        return;
    };
    let storage = SurrealStorage::connect(&db).await.unwrap();
    storage.migrate().await.unwrap();
    let now = chrono::Utc::now();
    storage
        .create_wave(WaveRecord {
            wave_id: "w-stored".into(),
            goal: "g".into(),
            status: "running".into(),
            orchestrator: "test".into(),
            created_at: now,
            closed_at: None,
            metadata: json!({"timeout_secs": i64::MAX}),
        })
        .await
        .unwrap();
    storage
        .create_wave_task(WaveTaskRecord {
            wave_id: "w-stored".into(),
            task_id: "t1".into(),
            worker: "w1".into(),
            goal: "g".into(),
            status: "running".into(),
            write_scope: vec!["src/a".into()],
            dependencies: vec![],
            handoff_path: None,
            verify_cmd: None,
            created_at: now,
            started_at: Some(now),
            completed_at: None,
            result: None,
            verify_result: None,
        })
        .await
        .unwrap();
}

// ── Item 8: only running tasks move; API updates go through the orchestrator

#[tokio::test]
async fn pending_task_results_are_ignored_and_api_updates_use_the_state_machine() {
    let _guard = STACK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        "w-order",
        vec![task("t1", "w1", &[], None), task("t2", "w2", &["t1"], None)],
    )
    .await;
    recv_session_start(&mut inbox_w1, "w-order", "t1", 30).await;

    // t2's worker "completes" t2 before it was ever dispatched: ignored.
    let w2 = HubClient::connect(&stack.nats_url, "w2").await.unwrap();
    emit_task_event(
        &w2,
        "w-order",
        "t2",
        "completed",
        json!({"result": "early"}),
    )
    .await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let snap = wave_status(&api, "w-order").await;
    assert_eq!(task_status(&snap, "t2"), "pending", "{snap}");

    // The API can't move a pending task of a running wave either...
    let early = json!({"wave_id": "w-order", "task_id": "t2", "status": "done"});
    assert!(api.request("wave.update_task_status", early).await.is_err());
    // ...but completing the running t1 through the API goes through the
    // state machine: t2 gets dispatched, t1's session is closed.
    api.request(
        "wave.update_task_status",
        json!({"wave_id": "w-order", "task_id": "t1", "status": "done", "result": "via api"}),
    )
    .await
    .expect("update running task");
    recv_action(&mut inbox_w1, "session_close", "w-order", "t1", 10).await;
    recv_session_start(&mut inbox_w2, "w-order", "t2", 10).await;
    let snap = wave_status(&api, "w-order").await;
    assert_eq!(task_status(&snap, "t1"), "done");
    assert_eq!(task_field(&snap, "t1", "result"), "via api");

    emit_task_event(&w2, "w-order", "t2", "completed", json!({"result": "ok"})).await;
    wait_status(&api, "w-order", 30, |s| s["wave"]["status"] == "completed").await;
    hub.kill().await.unwrap();
}

// ── Item 9: legacy presence can't keep a task alive in bound mode ─────

#[tokio::test]
async fn legacy_presence_is_ignored_when_bound_identity_is_required() {
    let _guard = STACK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(stack) = TestStack::new().await else {
        eprintln!("skipping: nats-server/hub-server binary not found");
        return;
    };
    let mut hub = stack.start_hub_with(LIVENESS_SECS, &["--require-bound-identity"]);
    // Bound mode: the API needs a bound caller (the wave's owner).
    let deadline = Instant::now() + Duration::from_secs(60);
    let api = loop {
        if let Ok(api) = ApiClient::connect_with_identity(&stack.nats_url, "test").await {
            if api.request("wave.list", json!({})).await.is_ok() {
                break api;
            }
        }
        assert!(Instant::now() < deadline, "hub-server never came up");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let raw = async_nats::connect(&stack.nats_url).await.unwrap();

    create_and_spawn(&api, "w-spoof", vec![task("t1", "w1", &[], None)]).await;
    // Someone forges w1's heartbeat on the legacy subject (identity from the
    // payload). The ghost w1 must still be failed by the liveness TTL.
    let spoof = async {
        loop {
            let env = nats_hub::Envelope::new(
                "mallory",
                "presence",
                nats_hub::MessageKind::Status,
                json!({"identity": "w1"}),
            );
            let _ = raw
                .publish("hub.presence", env.to_json_bytes().unwrap().into())
                .await;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    };
    let snap = tokio::select! {
        snap = wait_status(&api, "w-spoof", 30, |s| s["wave"]["status"] == "failed") => snap,
        _ = spoof => unreachable!(),
    };
    assert_eq!(task_status(&snap, "t1"), "failed");
    assert!(task_field(&snap, "t1", "result")
        .as_str()
        .unwrap_or("")
        .contains("liveness"));
    hub.kill().await.unwrap();
}

// ── Item 4 / C6: the Rust hub-worker completes (and cancels) wave tasks

#[tokio::test]
async fn rust_hub_worker_completes_a_wave_task() {
    let _guard = STACK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(stack) = TestStack::new().await else {
        eprintln!("skipping: nats-server/hub-server binary not found");
        return;
    };
    let mut hub = stack.start_hub();
    stack.wait_hub().await;
    let api = ApiClient::connect(&stack.nats_url).await.unwrap();
    let raw = async_nats::connect(&stack.nats_url).await.unwrap();
    let mut reg = raw.subscribe("hub.register.*").await.unwrap();
    raw.flush().await.unwrap();

    // `cat` echoes the prompt (hub-worker feeds it on stdin).
    let mut worker = spawn_hub_worker(&stack, "rw-1", "cat");
    wait_registered(&mut reg, "rw-1", &mut worker).await;

    let mut t = task("t1", "rw-1", &[], None);
    t["goal"] = json!("hello from the wave");
    create_and_spawn(&api, "w-rust", vec![t]).await;

    let snap = wait_status(&api, "w-rust", 30, |s| s["wave"]["status"] == "completed").await;
    assert_eq!(task_status(&snap, "t1"), "done");
    assert!(
        task_field(&snap, "t1", "result")
            .as_str()
            .unwrap_or("")
            .contains("hello from the wave"),
        "{snap}"
    );
    worker.kill().await.unwrap();
    hub.kill().await.unwrap();
}

#[tokio::test]
async fn wave_cancel_stops_a_rust_hub_worker_turn() {
    let _guard = STACK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(stack) = TestStack::new().await else {
        eprintln!("skipping: nats-server/hub-server binary not found");
        return;
    };
    let mut hub = stack.start_hub();
    stack.wait_hub().await;
    let api = ApiClient::connect(&stack.nats_url).await.unwrap();
    let raw = async_nats::connect(&stack.nats_url).await.unwrap();
    let mut reg = raw.subscribe("hub.register.*").await.unwrap();
    raw.flush().await.unwrap();

    let mut worker = spawn_hub_worker(&stack, "rw-2", "sleep 60");
    wait_registered(&mut reg, "rw-2", &mut worker).await;
    let ctl = HubClient::connect(&stack.nats_url, "testctl")
        .await
        .unwrap();
    let mut task_rx = ctl
        .subscribe_channel("wave.w-rcancel.task.t1")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    create_and_spawn(&api, "w-rcancel", vec![task("t1", "rw-2", &[], None)]).await;
    // Wait for the worker to start the command ("working" status).
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let env = tokio::time::timeout_at(deadline.into(), task_rx.recv())
            .await
            .expect("no working status from rw-2")
            .unwrap();
        if env.payload["status"] == "working" {
            break;
        }
    }

    api.request("wave.cancel", json!({"wave_id": "w-rcancel"}))
        .await
        .expect("wave.cancel");
    let result = recv_result(&mut task_rx, 15).await;
    assert_eq!(result.meta.from, "rw-2");
    assert_eq!(
        result.payload["status"], "cancelled",
        "{:?}",
        result.payload
    );
    assert!(
        worker.try_wait().unwrap().is_none(),
        "worker must survive a cancel"
    );
    worker.kill().await.unwrap();
    hub.kill().await.unwrap();
}

// ── Item 2 / C2: wave.cancel reaches a Python worker's running turn ────

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[tokio::test]
async fn wave_cancel_kills_a_python_worker_backend() {
    let _guard = STACK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let python = repo().join(".venv/bin/python");
    if !python.exists() || !Path::new("/proc/self/stat").exists() {
        eprintln!("skipping: needs the repo venv (make setup) and Linux /proc");
        return;
    }
    let Some(stack) = TestStack::new().await else {
        eprintln!("skipping: nats-server/hub-server binary not found");
        return;
    };
    // Python workers heartbeat every 30s: use a TTL longer than the test.
    let mut hub = stack.start_hub_with("120", &[]);
    stack.wait_hub().await;
    let api = ApiClient::connect(&stack.nats_url).await.unwrap();
    let raw = async_nats::connect(&stack.nats_url).await.unwrap();
    let mut reg = raw.subscribe("hub.register.*").await.unwrap();
    raw.flush().await.unwrap();

    let identity = "pyw-cancel";
    let pidfile = stack.tmp_path().join("claude.json");
    let fixtures = repo().join("tests/python/fixtures/bin");
    let path = format!(
        "{}:{}",
        fixtures.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut worker = Command::new(&python)
        .current_dir(repo())
        .args([
            "claude_worker.py",
            "--identity",
            identity,
            "--repo",
            stack.tmp_path().to_str().unwrap(),
            "--nats-url",
            &stack.nats_url,
            "--claude-bin",
            fixtures.join("claude").to_str().unwrap(),
        ])
        .env("PATH", path)
        .env("PYTHONUNBUFFERED", "1")
        .env("FAKE_CLAUDE_MODE", "sleep")
        .env("FAKE_CLAUDE_PIDFILE", &pidfile)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("python worker spawn");
    wait_registered(&mut reg, identity, &mut worker).await;

    let ctl = HubClient::connect(&stack.nats_url, "testctl")
        .await
        .unwrap();
    let mut task_rx = ctl
        .subscribe_channel("wave.w-pycancel.task.t1")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    create_and_spawn(&api, "w-pycancel", vec![task("t1", identity, &[], None)]).await;

    // The fake `claude` is running (it wrote its pid + its child's pid).
    let deadline = Instant::now() + Duration::from_secs(30);
    let pids: serde_json::Value = loop {
        if let Ok(text) = std::fs::read_to_string(&pidfile) {
            break serde_json::from_str(&text).unwrap();
        }
        assert!(Instant::now() < deadline, "fake claude never started");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let pids = [
        pids["pid"].as_u64().unwrap(),
        pids["child"].as_u64().unwrap(),
    ];
    assert!(pids.iter().all(|p| pid_alive(*p)));

    api.request("wave.cancel", json!({"wave_id": "w-pycancel"}))
        .await
        .expect("wave.cancel");
    let result = recv_result(&mut task_rx, 15).await;
    assert_eq!(result.meta.from, identity);
    assert_eq!(
        result.payload["status"], "cancelled",
        "{:?}",
        result.payload
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while pids.iter().any(|p| pid_alive(*p)) {
        assert!(
            Instant::now() < deadline,
            "backend process group not killed"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        worker.try_wait().unwrap().is_none(),
        "worker must survive a cancel"
    );
    worker.kill().await.unwrap();
    hub.kill().await.unwrap();
}
