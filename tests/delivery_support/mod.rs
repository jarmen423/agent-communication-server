//! Shared helpers for the durable-delivery tests: a private JetStream
//! nats-server (optionally with a config file) and restartable hub-servers.
//! Each test binary uses a subset, hence `dead_code`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use nats_hub::{Envelope, HubClient, MessageKind};
use serde_json::json;
use tempfile::TempDir;
use tokio::process::{Child, Command};

/// Private stacks start servers; one at a time keeps startup snappy.
pub static STACK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub fn nats_server_bin() -> Option<PathBuf> {
    let local = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tools/bin/nats-server");
    if local.exists() {
        return Some(local);
    }
    std::process::Command::new("nats-server")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()
        .filter(|s| s.success())
        .map(|_| PathBuf::from("nats-server"))
}

pub fn hub_server_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hub-server"))
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn unique(prefix: &str) -> String {
    format!(
        "{prefix}-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..10]
    )
}

/// A private nats-server with JetStream and hub-servers started on demand.
pub struct DurableStack {
    pub tmp: TempDir,
    nats: Child,
    pub port: u16,
    pub url: String,
}

impl DurableStack {
    /// Anonymous nats-server with JetStream (`-js`).
    pub async fn new() -> Option<Self> {
        Self::start(None).await
    }

    /// nats-server from `conf` (must enable JetStream itself and listen on
    /// `port`; see [`Self::port_for_config`]).
    pub async fn with_config(conf: &Path, port: u16) -> Option<Self> {
        Self::start(Some((conf, port))).await
    }

    async fn start(conf: Option<(&Path, u16)>) -> Option<Self> {
        let bin = nats_server_bin()?;
        let tmp = TempDir::new().ok()?;
        let mut cmd = Command::new(bin);
        let port = match conf {
            Some((path, port)) => {
                cmd.args(["-c", path.to_str()?]);
                port
            }
            None => {
                let port = free_port();
                let js = tmp.path().join("js");
                cmd.args(["-a", "127.0.0.1", "-p", &port.to_string(), "-js", "-sd"])
                    .arg(js);
                port
            }
        };
        let nats = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        for _ in 0..100 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Some(Self {
            tmp,
            nats,
            port,
            url: format!("nats://127.0.0.1:{port}"),
        })
    }

    /// hub-server with no storage (routing only) plus `extra` flags.
    pub fn hub(&self, extra: &[&str]) -> Child {
        self.hub_env(extra, &[])
    }

    pub fn hub_env(&self, extra: &[&str], env: &[(&str, &str)]) -> Child {
        let mut cmd = Command::new(hub_server_bin());
        cmd.args(["--nats-url", &self.url, "--db-path", ""])
            .args(extra);
        // `make test-bound`: the private hub enforces bound identity too.
        if std::env::var_os("NATS_HUB_REQUIRE_BOUND").is_some() {
            cmd.arg("--require-bound-identity");
        }
        cmd.env(
            "RUST_LOG",
            std::env::var("HUB_LOG").unwrap_or_else(|_| "warn".into()),
        )
        .kill_on_drop(true);
        for (k, v) in env {
            cmd.env(k, v);
        }
        if std::env::var("HUB_LOG").is_err() {
            // hub-server logs to stdout; keep both quiet unless asked.
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
        cmd.spawn().expect("spawn hub-server")
    }

    pub async fn durable_hub(&self) -> Child {
        let hub = self.hub(&["--durable"]);
        wait_routing(&self.url).await;
        hub
    }
}

impl Drop for DurableStack {
    fn drop(&mut self) {
        let _ = self.nats.start_kill();
    }
}

/// Block until a router routes: DM ourselves a probe until it comes back.
pub async fn wait_routing(url: &str) {
    let probe_id = unique("probe");
    let client = HubClient::connect(url, probe_id.as_str()).await.unwrap();
    let mut rx = client
        .subscribe_subject(&format!("channel.inbox.{probe_id}"))
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let env = Envelope::new(probe_id.as_str(), "probe", MessageKind::Control, json!({}))
            .to(&probe_id);
        client.send(&env).await.unwrap();
        if let Ok(Some(got)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
            if got.meta.from == probe_id {
                return;
            }
        }
    }
    panic!("router never started routing on {url}");
}

/// Collect envelopes from `rx` until `n` arrived or `within` passed.
pub async fn collect(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Envelope>,
    n: usize,
    within: Duration,
) -> Vec<Envelope> {
    let deadline = tokio::time::Instant::now() + within;
    let mut out = Vec::new();
    while out.len() < n {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(env)) => out.push(env),
            _ => break,
        }
    }
    out
}

/// Kill a child and wait for it to exit.
pub async fn stop(mut child: Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// [`wait_routing`] as a specific authenticated identity.
pub async fn wait_routing_as(url: &str, identity: &str, opts: nats_hub::HubConnectOptions) {
    let client = HubClient::connect_with_opts(url, identity, opts)
        .await
        .unwrap();
    let mut rx = client
        .subscribe_subject(&format!("channel.inbox.{identity}"))
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let env = Envelope::new(identity, "probe", MessageKind::Control, json!({})).to(identity);
        client.send(&env).await.unwrap();
        if let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
            let _ = client.drain().await;
            return;
        }
    }
    panic!("router never started routing on {url}");
}
