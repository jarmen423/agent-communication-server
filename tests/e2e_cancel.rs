//! End-to-end tests for the cancel contract (refocus-iteration-2.md §4.2)
//! and the `hub-delegate` CLI surface (stderr logs, `--prompt-file`,
//! `--prompt -`, Ctrl-C). Real binaries against a real router, so they only
//! run under `scripts/dev/with_stack.sh` (NATS_HUB_TEST_STACK + NATS_URL).

use nats_hub::client::is_task_result;
use nats_hub::{Envelope, HubClient, MessageKind};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
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

fn fixtures() -> PathBuf {
    repo_root().join("tests/python/fixtures")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(uniq("nats-hub-e2e-cancel"));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

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

/// Spawn a worker and wait for its `hub.register`.
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
    recv_until(&mut reg, 30, |e| e.payload["identity"] == identity).await;
    let _ = probe.drain().await;
    child
}

async fn spawn_rust_worker(url: &str, identity: &str, execute: &str) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hub-worker"));
    cmd.args([
        "--identity",
        identity,
        "--nats-url",
        url,
        "--execute",
        execute,
    ]);
    spawn_worker(url, identity, cmd).await
}

/// A Rust worker whose command hangs after spawning a grandchild `sleep`
/// (its pid lands in `pidfile`).
async fn spawn_hanging_rust_worker(url: &str, identity: &str, pidfile: &Path) -> Child {
    let misbehave = fixtures().join("misbehave.py");
    let exec = format!(
        "python3 {} sleep {}",
        misbehave.display(),
        pidfile.display()
    );
    spawn_rust_worker(url, identity, &exec).await
}

async fn wait_for_pid(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "{} never written",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// True while `pid` exists and is not a zombie (Linux /proc; else `kill -0`).
async fn pid_alive(pid: u32) -> bool {
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        let state = stat
            .rsplit(')')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .next();
        return !matches!(state, Some("Z") | Some("X") | None);
    }
    if Path::new("/proc/self").exists() {
        return false;
    }
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .await
        .is_ok_and(|s| s.success())
}

async fn wait_dead(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while pid_alive(pid).await {
        if Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    true
}

async fn sigint(child: &Child) {
    let pid = child.id().expect("child pid").to_string();
    Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .await
        .unwrap();
}

fn delegate_cmd(url: &str, worker: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hub-delegate"));
    cmd.args([
        "--to",
        worker,
        "--nats-url",
        url,
        "--from",
        &uniq("delegator"),
    ])
    .args(["--timeout", "60"])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    cmd
}

/// Run hub-delegate, wait until the worker has started the task (pidfile),
/// press Ctrl-C once, and return (exit code, stdout, stderr, elapsed).
async fn delegate_and_interrupt(
    url: &str,
    worker: &str,
    ready: impl std::future::Future<Output = ()>,
) -> (Option<i32>, String, String, Duration) {
    let child = delegate_cmd(url, worker)
        .args(["--prompt", "long job", "--verbose"])
        .spawn()
        .expect("spawn hub-delegate");
    ready.await;
    let started = Instant::now();
    sigint(&child).await;
    let out = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .expect("hub-delegate did not exit after Ctrl-C")
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        started.elapsed(),
    )
}

// ── hub-delegate CLI surface ──────────────────────────────────────────

// With RUST_LOG=info every log line goes to stderr; stdout is only the result.
#[tokio::test]
async fn e2e_delegate_logs_go_to_stderr() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("rev-worker");
    let _w = spawn_rust_worker(&url, &worker, "rev").await;
    let out = delegate_cmd(&url, &worker)
        .args(["--prompt", "hello nats"])
        .env("RUST_LOG", "info")
        .output()
        .await
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "stan olleh\n");
    assert!(
        stderr.contains("delegating task"),
        "INFO logs on stderr: {stderr}"
    );
}

// --prompt-file and --prompt - carry prompts past the 128 KiB argv limit.
#[tokio::test]
async fn e2e_delegate_prompt_file_and_stdin() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("cat-worker");
    let _w = spawn_rust_worker(&url, &worker, "cat").await;
    // 300 KiB: more than one argv string may hold (MAX_ARG_STRLEN = 128 KiB).
    let prompt: String = "0123456789abcdef".repeat(300 * 64);

    let file = scratch("prompt.txt");
    std::fs::write(&file, &prompt).unwrap();
    let out = delegate_cmd(&url, &worker)
        .arg("--prompt-file")
        .arg(&file)
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{prompt}\n"));

    let mut child = delegate_cmd(&url, &worker)
        .args(["--prompt", "-"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"from stdin").await.unwrap();
    drop(stdin);
    let out = child.wait_with_output().await.unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "from stdin\n");

    // Exactly one prompt source.
    let both = delegate_cmd(&url, &worker)
        .args(["--prompt", "x", "--prompt-file"])
        .arg(&file)
        .output()
        .await
        .unwrap();
    assert!(!both.status.success());
}

// ── Rust hub-worker cancel ────────────────────────────────────────────

#[tokio::test]
async fn e2e_rust_worker_cancel_kills_group_and_reports_cancelled() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("hang-worker");
    let pidfile = scratch("grandchild.pid");
    let _w = spawn_hanging_rust_worker(&url, &worker, &pidfile).await;

    let me = HubClient::connect(&url, uniq("canceller")).await.unwrap();
    let channel = uniq("task.cancel");
    let mut rx = me.subscribe_channel(&channel).await.unwrap();
    let send_task = |prompt: &str| {
        Envelope::new(
            me.identity(),
            &channel,
            MessageKind::Message,
            json!({"prompt": prompt, "task_channel": channel}),
        )
        .to(&worker)
    };
    let cancel = |id: &str| {
        Envelope::new(
            me.identity(),
            format!("inbox.{worker}"),
            MessageKind::Control,
            json!({"action": "cancel", "task_id": id}),
        )
        .to(&worker)
    };

    let running = send_task("first");
    me.send(&running).await.unwrap();
    let grandchild = wait_for_pid(&pidfile).await;
    let queued = send_task("second");
    me.send(&queued).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Unknown id: ignored. Queued task: cancelled at once, never runs.
    me.send(&cancel("no-such-task")).await.unwrap();
    me.send(&cancel(&queued.meta.id)).await.unwrap();
    let q = recv_until(&mut rx, 10, |e| is_task_result(e, &queued.meta.id)).await;
    assert_eq!(
        q.payload,
        json!({"status": "cancelled", "task_id": queued.meta.id, "result": null, "error": "cancelled"})
    );
    assert!(
        pid_alive(grandchild).await,
        "the running task must be untouched"
    );

    let started = Instant::now();
    me.send(&cancel(&running.meta.id)).await.unwrap();
    let r = recv_until(&mut rx, 10, |e| is_task_result(e, &running.meta.id)).await;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(r.meta.reply_to.as_deref(), Some(running.meta.id.as_str()));
    assert_eq!(r.payload["status"], "cancelled");
    assert!(wait_dead(grandchild).await, "process group not killed");
    let st = recv_until(&mut rx, 5, |e| {
        e.meta.kind == MessageKind::Status && e.meta.reply_to.as_deref() == Some(&running.meta.id)
    })
    .await;
    assert!(["working", "cancelled"].contains(&st.payload["status"].as_str().unwrap_or("")));
    let _ = me.drain().await;
}

// ── hub-delegate Ctrl-C ───────────────────────────────────────────────

#[tokio::test]
async fn e2e_delegate_ctrl_c_cancels_rust_worker() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("hang-worker");
    let pidfile = scratch("grandchild.pid");
    let _w = spawn_hanging_rust_worker(&url, &worker, &pidfile).await;

    let (code, stdout, stderr, took) = delegate_and_interrupt(&url, &worker, async {
        wait_for_pid(&pidfile).await;
    })
    .await;
    assert_eq!(code, Some(4), "exit 4 = cancelled; stderr: {stderr}");
    assert_eq!(stdout, "", "stdout carries only a result");
    assert!(stderr.contains("cancelled"), "{stderr}");
    assert!(took < Duration::from_secs(10));
    assert!(wait_dead(wait_for_pid(&pidfile).await).await);
}

#[tokio::test]
async fn e2e_delegate_ctrl_c_cancels_python_worker() {
    let Some(url) = stack_url() else { return };
    let python = repo_root().join(".venv/bin/python");
    if !python.exists() {
        eprintln!("Skipping — .venv/bin/python missing (run make setup)");
        return;
    }
    let worker = uniq("py-claude");
    let pidfile = scratch("claude.json");
    let mut cmd = Command::new(python);
    cmd.args([
        "claude_worker.py",
        "--identity",
        &worker,
        "--nats-url",
        &url,
    ])
    .arg("--claude-bin")
    .arg(fixtures().join("bin/claude"))
    .arg("--repo")
    .arg(std::env::temp_dir())
    .env("FAKE_CLAUDE_MODE", "sleep")
    .env("FAKE_CLAUDE_PIDFILE", &pidfile);
    let _w = spawn_worker(&url, &worker, cmd).await;

    let (code, stdout, stderr, took) = delegate_and_interrupt(&url, &worker, async {
        wait_for_pid_json(&pidfile).await;
    })
    .await;
    assert_eq!(code, Some(4), "exit 4 = cancelled; stderr: {stderr}");
    assert_eq!(stdout, "");
    assert!(took < Duration::from_secs(10));
    let (pid, child) = wait_for_pid_json(&pidfile).await;
    assert!(
        wait_dead(pid).await && wait_dead(child).await,
        "claude group not killed"
    );
}

async fn wait_for_pid_json(path: &Path) -> (u32, u32) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            return (
                v["pid"].as_u64().unwrap() as u32,
                v["child"].as_u64().unwrap() as u32,
            );
        }
        assert!(
            Instant::now() < deadline,
            "{} never written",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// Nobody answers the cancel: the second Ctrl-C exits at once (130).
#[tokio::test]
async fn e2e_delegate_second_ctrl_c_exits_immediately() {
    let Some(url) = stack_url() else { return };
    let child = delegate_cmd(&url, &uniq("nobody"))
        .args(["--prompt", "into the void"])
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    sigint(&child).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let started = Instant::now();
    sigint(&child).await;
    let out = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .expect("second Ctrl-C must exit at once")
        .unwrap();
    assert_eq!(out.status.code(), Some(130));
    assert!(started.elapsed() < Duration::from_secs(3));
}
