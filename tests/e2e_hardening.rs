//! H3 hardening (.planning/refocus-2/HARDENING.md) end to end, real binaries
//! against a real router — only under `scripts/dev/with_stack.sh`:
//!
//! - `hub-worker` ignores a cancel from a third party (C5), bounds its queue,
//!   and kills the running command's process group on SIGTERM;
//! - `hub-delegate` cancels the task when it times out, and ignores a result
//!   from anyone but the worker it delegated to (C5).

mod support;

use nats_hub::client::is_task_result;
use nats_hub::{Envelope, HubClient, MessageKind};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedReceiver;

type Rx = UnboundedReceiver<Envelope>;

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

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(uniq("nats-hub-e2e-hardening"));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

async fn recv_until(rx: &mut Rx, secs: u64, pred: impl Fn(&Envelope) -> bool) -> Envelope {
    try_recv_until(rx, Duration::from_secs(secs), pred)
        .await
        .unwrap_or_else(|| panic!("no matching envelope within {secs}s"))
}

async fn try_recv_until(
    rx: &mut Rx,
    within: Duration,
    pred: impl Fn(&Envelope) -> bool,
) -> Option<Envelope> {
    let wait = async {
        loop {
            let env = rx.recv().await.expect("subscription closed");
            if pred(&env) {
                return env;
            }
        }
    };
    tokio::time::timeout(within, wait).await.ok()
}

/// A `hub-worker` whose command spawns a grandchild `sleep` (pid → `pidfile`)
/// and hangs; waits for its `hub.register`.
async fn spawn_hanging_worker(url: &str, identity: &str, pidfile: &Path, extra: &[&str]) -> Child {
    let misbehave = repo_root().join("tests/python/fixtures/misbehave.py");
    let exec = format!(
        "python3 {} sleep {}",
        misbehave.display(),
        pidfile.display()
    );
    let probe = HubClient::connect(url, uniq("probe")).await.unwrap();
    let mut reg = probe.subscribe_subject("hub.register.*").await.unwrap();
    let child = Command::new(support::client_bin("hub-worker"))
        .args([
            "--identity",
            identity,
            "--nats-url",
            url,
            "--execute",
            &exec,
        ])
        .args(extra)
        .current_dir(repo_root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn hub-worker");
    recv_until(&mut reg, 30, |e| e.payload["identity"] == identity).await;
    let _ = probe.drain().await;
    child
}

async fn wait_for_pid(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(pid) = std::fs::read_to_string(path)
            .unwrap_or_default()
            .trim()
            .parse()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "{} never written",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// True while `pid` exists and is not a zombie (Linux /proc).
fn pid_alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => {
            let state = stat
                .rsplit(')')
                .next()
                .unwrap_or("")
                .split_whitespace()
                .next();
            !matches!(state, Some("Z") | Some("X") | None)
        }
        Err(_) => false,
    }
}

async fn wait_dead(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_alive(pid) {
        if Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    true
}

fn task(me: &HubClient, worker: &str, channel: &str, prompt: &str) -> Envelope {
    Envelope::new(
        me.identity(),
        channel,
        MessageKind::Message,
        json!({"prompt": prompt, "task_channel": channel}),
    )
    .to(worker)
}

fn cancel(me: &HubClient, worker: &str, task_id: &str) -> Envelope {
    Envelope::new(
        me.identity(),
        format!("inbox.{worker}"),
        MessageKind::Control,
        json!({"action": "cancel", "task_id": task_id}),
    )
    .to(worker)
}

// ── hub-worker ────────────────────────────────────────────────────────

#[tokio::test]
async fn e2e_rust_worker_ignores_third_party_cancel_and_bounds_queue() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("hang-worker");
    let pidfile = scratch("grandchild.pid");
    let _w = spawn_hanging_worker(&url, &worker, &pidfile, &["--max-queued", "1"]).await;

    let boss = HubClient::connect(&url, uniq("boss")).await.unwrap();
    let mallory = HubClient::connect(&url, uniq("mallory")).await.unwrap();
    let channel = uniq("task.hard");
    let mut rx = boss.subscribe_channel(&channel).await.unwrap();

    let running = task(&boss, &worker, &channel, "first");
    boss.send(&running).await.unwrap();
    let grandchild = wait_for_pid(&pidfile).await;
    let queued = task(&boss, &worker, &channel, "second");
    boss.send(&queued).await.unwrap();
    let overflow = task(&boss, &worker, &channel, "third");
    boss.send(&overflow).await.unwrap();

    // The queue holds one task: the third gets an error result at once.
    let full = recv_until(&mut rx, 10, |e| {
        is_task_result(e, &overflow.meta.id, &worker)
    })
    .await;
    assert_eq!(full.payload["status"], "error");
    assert!(
        full.payload["error"]
            .as_str()
            .unwrap_or("")
            .contains("busy"),
        "{full:?}"
    );

    // Someone else's cancel is ignored, for the running and the queued task.
    mallory
        .send(&cancel(&mallory, &worker, &running.meta.id))
        .await
        .unwrap();
    mallory
        .send(&cancel(&mallory, &worker, &queued.meta.id))
        .await
        .unwrap();
    let early = try_recv_until(&mut rx, Duration::from_millis(1500), |e| {
        e.meta.kind == MessageKind::Message
            && [running.meta.id.as_str(), queued.meta.id.as_str()]
                .contains(&e.meta.reply_to.as_deref().unwrap_or(""))
    })
    .await;
    assert!(early.is_none(), "third-party cancel honoured: {early:?}");
    assert!(
        pid_alive(grandchild),
        "third-party cancel hit the running task"
    );

    // The sender's own cancel works.
    boss.send(&cancel(&boss, &worker, &running.meta.id))
        .await
        .unwrap();
    let r = recv_until(&mut rx, 10, |e| {
        is_task_result(e, &running.meta.id, &worker)
    })
    .await;
    assert_eq!(r.payload["status"], "cancelled");
    assert!(wait_dead(grandchild).await, "process group not killed");
    let _ = boss.drain().await;
    let _ = mallory.drain().await;
}

#[tokio::test]
async fn e2e_rust_worker_sigterm_kills_running_group() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("term-worker");
    let pidfile = scratch("grandchild.pid");
    let mut w = spawn_hanging_worker(&url, &worker, &pidfile, &[]).await;

    let boss = HubClient::connect(&url, uniq("boss")).await.unwrap();
    let channel = uniq("task.term");
    boss.send(&task(&boss, &worker, &channel, "long"))
        .await
        .unwrap();
    let grandchild = wait_for_pid(&pidfile).await;

    let pid = w.id().expect("worker pid").to_string();
    let killed = Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .await
        .unwrap();
    assert!(killed.success());
    let status = tokio::time::timeout(Duration::from_secs(10), w.wait())
        .await
        .expect("hub-worker did not exit on SIGTERM")
        .unwrap();
    assert!(status.success(), "clean shutdown on SIGTERM: {status:?}");
    assert!(
        wait_dead(grandchild).await,
        "command's group outlived the worker"
    );
    let _ = boss.drain().await;
}

// ── hub-delegate ──────────────────────────────────────────────────────

#[tokio::test]
async fn e2e_delegate_timeout_cancels_the_task() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("hang-worker");
    let pidfile = scratch("grandchild.pid");
    let _w = spawn_hanging_worker(&url, &worker, &pidfile, &[]).await;

    let child = Command::new(support::client_bin("hub-delegate"))
        .args([
            "--to",
            &worker,
            "--nats-url",
            &url,
            "--from",
            &uniq("delegator"),
        ])
        .args(["--timeout", "3", "--prompt", "long job"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let grandchild = wait_for_pid(&pidfile).await;
    let out = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .expect("hub-delegate hung")
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "exit 2 = timeout; {stderr}");
    assert!(stderr.contains("cancel sent"), "{stderr}");
    assert!(
        wait_dead(grandchild).await,
        "the timed-out task kept running"
    );
}

#[tokio::test]
async fn e2e_delegate_ignores_result_from_non_worker() {
    let Some(url) = stack_url() else { return };
    let worker_id = uniq("fake-worker");
    // Act as the worker by hand, so the test controls who answers.
    let worker = HubClient::connect(&url, &worker_id).await.unwrap();
    let mut inbox = worker.subscribe_inbox().await.unwrap();
    let mallory = HubClient::connect(&url, uniq("mallory")).await.unwrap();

    let child = Command::new(support::client_bin("hub-delegate"))
        .args([
            "--to",
            &worker_id,
            "--nats-url",
            &url,
            "--from",
            &uniq("delegator"),
        ])
        .args(["--timeout", "20", "--prompt", "hello"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let t = recv_until(&mut inbox, 15, |e| e.payload.get("task_channel").is_some()).await;
    let channel = t.payload["task_channel"].as_str().unwrap().to_string();
    let result = |from: &HubClient, text: &str| {
        Envelope::new(
            from.identity(),
            &channel,
            MessageKind::Message,
            json!({"status": "done", "task_id": t.meta.id, "result": text, "error": null}),
        )
        .reply_to(&t.meta.id)
    };
    mallory.send(&result(&mallory, "forged")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    worker.send(&result(&worker, "real")).await.unwrap();

    let out = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .expect("hub-delegate hung")
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "real\n");
    assert!(stderr.contains("ignoring a result"), "{stderr}");
    let _ = worker.drain().await;
    let _ = mallory.drain().await;
}

// ── hub-worker session actions (C5) ───────────────────────────────────

#[tokio::test]
async fn e2e_rust_worker_ignores_session_send_from_non_owner() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("sess-worker");
    let probe = HubClient::connect(&url, uniq("probe")).await.unwrap();
    let mut reg = probe.subscribe_subject("hub.register.*").await.unwrap();
    let _w = Command::new(support::client_bin("hub-worker"))
        .args([
            "--identity",
            &worker,
            "--nats-url",
            &url,
            "--execute",
            "rev",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    recv_until(&mut reg, 30, |e| e.payload["identity"] == worker.as_str()).await;

    let boss = HubClient::connect(&url, uniq("boss")).await.unwrap();
    let mallory = HubClient::connect(&url, uniq("mallory")).await.unwrap();
    let sid = uniq("s");
    let channel = format!("session.{sid}");
    let mut boss_in = boss.subscribe_inbox().await.unwrap();
    let mut mallory_in = mallory.subscribe_inbox().await.unwrap();
    let act = |from: &HubClient, action: &str, text: &str| {
        Envelope::new(
            from.identity(),
            &channel,
            MessageKind::Message,
            json!({"action": action, "session_id": sid, "message": text, "prompt": text}),
        )
        .to(&worker)
    };

    let start = act(&boss, "session_start", "abc");
    boss.send(&start).await.unwrap();
    let r = recv_until(&mut boss_in, 15, |e| {
        is_task_result(e, &start.meta.id, &worker)
    })
    .await;
    assert_eq!(r.payload["result"], "cba");

    // Someone else's turn on boss's session never runs.
    let forged = act(&mallory, "session_send", "evil");
    mallory.send(&forged).await.unwrap();
    let answered = try_recv_until(&mut mallory_in, Duration::from_secs(2), |e| {
        e.meta.reply_to.as_deref() == Some(forged.meta.id.as_str())
    })
    .await;
    assert!(
        answered.is_none(),
        "non-owner session_send ran: {answered:?}"
    );

    let send = act(&boss, "session_send", "xyz");
    boss.send(&send).await.unwrap();
    let r = recv_until(&mut boss_in, 15, |e| {
        is_task_result(e, &send.meta.id, &worker)
    })
    .await;
    assert_eq!(r.payload["result"], "zyx");
    let _ = probe.drain().await;
    let _ = boss.drain().await;
    let _ = mallory.drain().await;
}
