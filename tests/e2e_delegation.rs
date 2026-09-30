//! End-to-end delegation tests for the reply contract (refocus.md §6).
//!
//! These spawn the real `hub-worker` / `hub-delegate` binaries (and the Python
//! `echo_worker.py`) against a real router, so they only run under
//! `scripts/dev/with_stack.sh`, which sets `NATS_HUB_TEST_STACK=1` and
//! `NATS_URL`. Without it every test skips.

use nats_hub::client::is_task_result;
use nats_hub::{ApiClient, Envelope, HubClient, MessageKind};
use serde_json::json;
use std::path::PathBuf;
use std::process::{Output, Stdio};
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedReceiver;

type Rx = UnboundedReceiver<Envelope>;

/// `NATS_URL` when running under with_stack.sh, else `None` (skip).
fn stack_url() -> Option<String> {
    if std::env::var_os("NATS_HUB_TEST_STACK").is_none() {
        eprintln!("Skipping — needs scripts/dev/with_stack.sh (NATS_HUB_TEST_STACK unset)");
        return None;
    }
    Some(std::env::var("NATS_URL").expect("with_stack.sh exports NATS_URL"))
}

fn uniq(prefix: &str) -> String {
    format!(
        "{prefix}-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    )
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Receive the next envelope matching `pred`, failing after `secs`.
async fn recv_until(rx: &mut Rx, secs: u64, pred: impl Fn(&Envelope) -> bool) -> Envelope {
    let wait = async {
        loop {
            let env = rx.recv().await.expect("subscription closed");
            if pred(&env) {
                return env;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(secs), wait)
        .await
        .unwrap_or_else(|_| panic!("no matching envelope within {secs}s"))
}

/// Spawn a worker process and wait until it announces itself on
/// `hub.register` (it subscribes to its inbox before registering).
async fn spawn_worker(url: &str, identity: &str, mut cmd: Command) -> Child {
    let probe = HubClient::connect(url, uniq("probe")).await.unwrap();
    let mut reg = probe.subscribe_subject("hub.register").await.unwrap();
    let child = cmd
        .current_dir(repo_root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn worker");
    recv_until(&mut reg, 20, |e| e.payload["identity"] == identity).await;
    let _ = probe.drain().await;
    child
}

async fn spawn_rust_worker(url: &str, identity: &str, execute: &str, extra: &[&str]) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hub-worker"));
    cmd.args([
        "--identity",
        identity,
        "--nats-url",
        url,
        "--execute",
        execute,
    ])
    .args(extra);
    spawn_worker(url, identity, cmd).await
}

/// Run `hub-delegate` while recording every envelope on `channel.>` that the
/// worker or the delegator sends.
async fn delegate_and_observe(url: &str, worker: &str, prompt: &str) -> (Output, Vec<Envelope>) {
    let from = uniq("delegator");
    let observer = HubClient::connect(url, uniq("observer")).await.unwrap();
    let mut all = observer.subscribe_all().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let out = Command::new(env!("CARGO_BIN_EXE_hub-delegate"))
        .args(["--to", worker, "--prompt", prompt, "--nats-url", url])
        .args(["--from", &from, "--timeout", "30", "--verbose"])
        .output()
        .await
        .expect("run hub-delegate");

    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut seen = Vec::new();
    while let Ok(env) = all.try_recv() {
        if env.meta.from == worker || env.meta.from == from {
            seen.push(env);
        }
    }
    let _ = observer.drain().await;
    (out, seen)
}

/// Assert the observed traffic follows §6: the task carries task_channel and
/// no reply_to; every worker envelope on the task channel correlates to the
/// task id; exactly one terminal result.
fn assert_contract(seen: &[Envelope], worker: &str, expect_result: &str) {
    let task = seen
        .iter()
        .find(|e| e.meta.to.as_deref() == Some(worker))
        .expect("task envelope observed");
    let task_id = task.meta.id.as_str();
    let channel = task.payload["task_channel"].as_str().expect("task_channel");
    assert_eq!(task.meta.reply_to, None, "task must not carry reply_to");

    let from_worker: Vec<_> = seen.iter().filter(|e| e.meta.from == worker).collect();
    assert!(
        from_worker
            .iter()
            .any(|e| e.meta.kind == MessageKind::Status),
        "worker published no status"
    );
    for env in &from_worker {
        assert_eq!(env.meta.channel, channel, "worker left the task channel");
        assert_eq!(env.meta.to, None, "task-channel traffic is broadcast");
        assert_eq!(env.meta.reply_to.as_deref(), Some(task_id), "{env:?}");
    }
    let results: Vec<_> = from_worker
        .iter()
        .filter(|e| is_task_result(e, task_id))
        .collect();
    assert_eq!(results.len(), 1, "exactly one terminal result");
    assert_eq!(
        results[0].payload,
        json!({"status": "done", "task_id": task_id, "result": expect_result, "error": null})
    );
}

// (a) hub-delegate ↔ Rust hub-worker round trip.
#[tokio::test]
async fn e2e_delegate_to_rust_worker() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("rev-worker");
    let _child = spawn_rust_worker(&url, &worker, "rev", &[]).await;

    let (out, seen) = delegate_and_observe(&url, &worker, "hello nats").await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "hub-delegate failed: {stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "stan olleh\n");
    assert!(stderr.contains("[status]"), "--verbose progress: {stderr}");
    assert_contract(&seen, &worker, "stan olleh");
}

// (b) hub-delegate ↔ Python echo_worker.py (worker_runtime).
#[tokio::test]
async fn e2e_delegate_to_python_echo_worker() {
    let Some(url) = stack_url() else { return };
    let python = repo_root().join(".venv/bin/python");
    if !python.exists() {
        eprintln!("Skipping — .venv/bin/python missing (run make setup)");
        return;
    }
    let worker = uniq("echo");
    let mut cmd = Command::new(python);
    cmd.args(["echo_worker.py", "--identity", &worker, "--nats-url", &url]);
    let _child = spawn_worker(&url, &worker, cmd).await;

    let (out, seen) = delegate_and_observe(&url, &worker, "hello").await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "hub-delegate failed: {stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "echo: olleh\n");
    assert!(stderr.contains("[event]"), "--verbose events: {stderr}");
    assert_contract(&seen, &worker, "echo: olleh");
}

// (c) A plain DM without task_channel gets a DM reply (rule 6).
#[tokio::test]
async fn e2e_plain_dm_gets_dm_reply() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("cat-worker");
    let _child = spawn_rust_worker(&url, &worker, "cat", &[]).await;

    let me = HubClient::connect(&url, uniq("dm-sender")).await.unwrap();
    let mut inbox = me.subscribe_inbox().await.unwrap();
    let id = me
        .send_to(&worker, "chat.e2e", json!({"prompt": "plain dm"}))
        .await
        .unwrap();

    let reply = recv_until(&mut inbox, 15, |e| e.meta.from == worker).await;
    assert_eq!(reply.meta.kind, MessageKind::Message);
    assert_eq!(reply.meta.reply_to.as_deref(), Some(id.as_str()));
    assert_eq!(reply.meta.to.as_deref(), Some(me.identity()));
    assert_eq!(
        reply.payload,
        json!({"status": "done", "task_id": id, "result": "plain dm", "error": null})
    );
    let _ = me.drain().await;
}

/// Send a task with a task channel directly (bypassing hub-delegate) and
/// return (task id, terminal result).
async fn run_task(url: &str, worker: &str, prompt: &str, secs: u64) -> (String, Envelope) {
    let me = HubClient::connect(url, uniq("tasker")).await.unwrap();
    let channel = uniq("task.e2e");
    let mut rx = me.subscribe_channel(&channel).await.unwrap();
    let task = Envelope::new(
        me.identity(),
        &channel,
        MessageKind::Message,
        json!({"prompt": prompt, "task_channel": channel}),
    )
    .to(worker);
    let id = task.meta.id.clone();
    me.send(&task).await.unwrap();
    let result = recv_until(&mut rx, secs, |e| is_task_result(e, &id)).await;
    let _ = me.drain().await;
    (id, result)
}

// (d) A worker timeout produces an `error` result.
#[tokio::test]
async fn e2e_worker_timeout_reports_error() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("slow-worker");
    let _child = spawn_rust_worker(&url, &worker, "sleep 30", &["--timeout-secs", "1"]).await;

    let started = std::time::Instant::now();
    let (id, result) = run_task(&url, &worker, "anything", 15).await;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "kill took too long"
    );
    assert_eq!(result.meta.reply_to.as_deref(), Some(id.as_str()));
    assert_eq!(result.payload["status"], "error");
    assert_eq!(result.payload["task_id"], id.as_str());
    assert!(result.payload["result"].is_null());
    let err = result.payload["error"].as_str().unwrap_or_default();
    assert!(err.contains("timed out"), "error: {err}");
}

// (e) A 1 MB prompt doesn't deadlock (the old worker wrote all of stdin
// before reading stdout, so anything over the ~64 KiB pipe buffer hung):
// stdin is streamed while stdout is drained.
#[tokio::test]
async fn e2e_one_megabyte_prompt_does_not_deadlock() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("big-worker");
    let _child = spawn_rust_worker(&url, &worker, "cat", &[]).await;

    // 1,000,000 bytes, no characters JSON would escape, so the envelope
    // stays under NATS' default 1 MiB max_payload.
    let prompt: String = "0123456789abcdef".repeat(62_500);
    assert_eq!(prompt.len(), 1_000_000);
    let (_, result) = run_task(&url, &worker, &prompt, 30).await;
    assert_eq!(
        result.payload["status"], "done",
        "{:?}",
        result.payload["error"]
    );
    assert_eq!(result.payload["result"].as_str(), Some(prompt.as_str()));
}

// Registration is immediate: the worker is queryable within 3s of starting.
#[tokio::test]
async fn e2e_rust_worker_listed_in_agents_quickly() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("listed-worker");
    let started = std::time::Instant::now();
    let _child = spawn_rust_worker(&url, &worker, "cat", &[]).await;

    let api = ApiClient::connect(&url).await.unwrap();
    let filter = json!({"capabilities": [], "alive_within_secs": null, "limit": null});
    loop {
        let data = api.request("agent.find", filter.clone()).await.unwrap();
        let listed = data["agents"]
            .as_array()
            .is_some_and(|a| a.iter().any(|r| r["identity"] == worker.as_str()));
        if listed {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{worker} not listed within 3s: {data}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// A registration published before hub-server listens is lost (core NATS), so
// the worker re-announces quickly: a second `hub.register` within ~3s.
#[tokio::test]
async fn e2e_rust_worker_reannounces_registration() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("reannounce-worker");
    let _child = spawn_rust_worker(&url, &worker, "cat", &[]).await;

    // spawn_rust_worker consumed the first registration; expect another.
    let probe = HubClient::connect(&url, uniq("probe")).await.unwrap();
    let mut reg = probe.subscribe_subject("hub.register").await.unwrap();
    recv_until(&mut reg, 3, |e| e.payload["identity"] == worker.as_str()).await;
    let _ = probe.drain().await;
}
