//! Shared helpers for the live wave orchestrator tests: a private
//! nats-server + (re)startable hub-server, wave API helpers, and worker
//! inbox helpers. Each test binary uses a subset, hence `dead_code`.
#![allow(dead_code)]

use nats_hub::protocol::subjects;
use nats_hub::wave::wave_task_session_id;
use nats_hub::{ApiClient, Envelope, HubClient, MessageKind};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::process::{Child, Command};

pub const LIVENESS_SECS: &str = "3";

/// Live tests each boot a private nats-server + hub-server (each with an
/// embedded SurrealDB); serialize them so parallel stacks don't starve
/// each other during startup.
pub static STACK_LOCK: Mutex<()> = Mutex::new(());

pub fn nats_server_bin() -> Option<PathBuf> {
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

pub fn hub_server_bin() -> Option<PathBuf> {
    if let Some(p) = option_env!("CARGO_BIN_EXE_hub-server") {
        return Some(PathBuf::from(p));
    }
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
    let p = target.join("debug/hub-server");
    p.exists().then_some(p)
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A private nats-server + a (re)startable hub-server on its own DB.
pub struct TestStack {
    _tmp: TempDir,
    nats: Child,
    pub db_dir: PathBuf,
    pub nats_url: String,
}

impl TestStack {
    pub async fn new() -> Option<Self> {
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

    pub fn tmp_path(&self) -> &std::path::Path {
        self._tmp.path()
    }

    pub fn start_hub(&self) -> Child {
        self.start_hub_with(LIVENESS_SECS, &[])
    }

    /// Start hub-server with a given liveness TTL and extra flags.
    pub fn start_hub_with(&self, liveness_secs: &str, extra: &[&str]) -> Child {
        Command::new(hub_server_bin().unwrap())
            .args([
                "--nats-url",
                &self.nats_url,
                "--db-path",
                self.db_dir.to_str().unwrap(),
                "--wave-liveness-secs",
                liveness_secs,
            ])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("hub-server spawn")
    }

    /// Poll until the query API answers (hub-server fully up).
    pub async fn wait_hub(&self) {
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
pub async fn create_and_spawn(api: &ApiClient, wave_id: &str, tasks: Vec<Value>) {
    create_wave(api, wave_id, tasks).await;
    api.request(
        "wave.spawn",
        json!({"wave_id": wave_id, "timeout_secs": 60}),
    )
    .await
    .expect("wave.spawn");
}

/// Create (but don't spawn) a wave owned by `test`.
pub async fn create_wave(api: &ApiClient, wave_id: &str, tasks: Vec<Value>) {
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
}

pub fn task(task_id: &str, worker: &str, deps: &[&str], verify_cmd: Option<&str>) -> Value {
    json!({
        "task_id": task_id,
        "worker": worker,
        "goal": format!("goal {task_id}"),
        "write_scope": [format!("src/{task_id}")],
        "dependencies": deps,
        "verify_cmd": verify_cmd,
    })
}

pub async fn wave_status(api: &ApiClient, wave_id: &str) -> Value {
    api.request("wave.status", json!({"wave_id": wave_id}))
        .await
        .expect("wave.status")
}

/// Poll `wave.status` until `pred` holds (or panic at `secs`).
pub async fn wait_status<F>(api: &ApiClient, wave_id: &str, secs: u64, pred: F) -> Value
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

pub fn task_status(snap: &Value, task_id: &str) -> String {
    snap["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task_id"] == task_id)
        .map(|t| t["status"].as_str().unwrap_or("?").to_string())
        .unwrap_or_else(|| "missing".into())
}

pub fn task_field<'a>(snap: &'a Value, task_id: &str, field: &str) -> &'a Value {
    &snap["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task_id"] == task_id)
        .unwrap()[field]
}

/// Wait for a `session_start` DM for `wave_id`/`task_id` on a worker inbox
/// receiver. Wave task sessions are `<wave_id>--<task_id>` (contract C4).
pub async fn recv_session_start(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Envelope>,
    wave_id: &str,
    task_id: &str,
    secs: u64,
) -> Envelope {
    recv_action(rx, "session_start", wave_id, task_id, secs).await
}

/// Wait for a DM with `payload.action == action` for the wave task.
pub async fn recv_action(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Envelope>,
    action: &str,
    wave_id: &str,
    task_id: &str,
    secs: u64,
) -> Envelope {
    let session_id = wave_task_session_id(wave_id, task_id);
    let deadline = Instant::now() + Duration::from_secs(secs);
    while let Ok(Some(env)) = tokio::time::timeout(
        deadline.saturating_duration_since(Instant::now()),
        rx.recv(),
    )
    .await
    {
        if env.payload.get("action").and_then(|v| v.as_str()) == Some(action)
            && env.payload.get("session_id").and_then(|v| v.as_str()) == Some(&session_id)
        {
            return env;
        }
    }
    panic!("no {action} for {session_id} within {secs}s");
}

/// Publish a worker event on a task channel (`event_type`, `data`).
pub async fn emit_task_event(
    client: &HubClient,
    wave_id: &str,
    task_id: &str,
    ty: &str,
    data: Value,
) {
    let channel = subjects::wave_task_channel_name(wave_id, task_id);
    let env = Envelope::new(
        client.identity(),
        channel,
        MessageKind::Event,
        json!({"event_type": ty, "data": data}),
    );
    client.send(&env).await.unwrap();
}

/// Wait (up to `secs`) for a `kind=message` terminal result on a task channel.
pub async fn recv_result(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Envelope>,
    secs: u64,
) -> Envelope {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while let Ok(Some(env)) = tokio::time::timeout(
        deadline.saturating_duration_since(Instant::now()),
        rx.recv(),
    )
    .await
    {
        if env.meta.kind == MessageKind::Message && env.payload.get("status").is_some() {
            return env;
        }
    }
    panic!("no terminal result within {secs}s");
}
