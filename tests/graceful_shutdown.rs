//! Real signal/restart checks. A private NATS server avoids a second router
//! consuming our ingress in durable mode. Run via scripts/dev/with_stack.sh.
#![cfg(all(unix, feature = "storage-surreal"))]

mod delivery_support;

use delivery_support::{free_port, hub_server_bin, nats_server_bin, wait_routing, STACK_LOCK};
use futures_util::StreamExt;
use nats_hub::{ApiClient, Envelope, MessageKind};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::process::{Child, Command};

struct Stack {
    tmp: TempDir,
    _nats: Child,
    url: String,
}

async fn signal(child: &Child, name: &str) {
    assert!(Command::new("kill")
        .args([name, &child.id().expect("child running").to_string()])
        .status()
        .await
        .unwrap()
        .success());
}

impl Stack {
    async fn new() -> Option<Self> {
        if std::env::var_os("NATS_URL").is_none() {
            eprintln!("Skipping: run with scripts/dev/with_stack.sh");
            return None;
        }
        let tmp = tempfile::tempdir().unwrap();
        let port = free_port();
        let nats = Command::new(nats_server_bin().expect("make setup"))
            .args(["-a", "127.0.0.1", "-p", &port.to_string(), "-js", "-sd"])
            .arg(tmp.path().join("js"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let url = format!("nats://127.0.0.1:{port}");
        for _ in 0..100 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
                return Some(Self {
                    tmp,
                    _nats: nats,
                    url,
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("private NATS failed to start");
    }

    fn hub(&self, name: &str, durable: bool, extra: &[&str]) -> (Child, PathBuf) {
        let log_path = self.tmp.path().join(format!("{name}.log"));
        let log = std::fs::File::create(&log_path).unwrap();
        let mut cmd = Command::new(hub_server_bin());
        cmd.args(["--nats-url", &self.url, "--db-path"])
            .arg(self.tmp.path().join("db"))
            .args(extra)
            .env("RUST_LOG", "nats_hub=info")
            .env("NO_COLOR", "1")
            .env("SURREAL_ROCKSDB_STORAGE_LOG_LEVEL", "info")
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true);
        if durable {
            cmd.arg("--durable");
        }
        if std::env::var_os("NATS_HUB_REQUIRE_BOUND").is_some() {
            cmd.args(["--require-bound-identity", "--api-admin", "test-admin"]);
        }
        (cmd.spawn().unwrap(), log_path)
    }
}

fn logs(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

async fn wait_log(child: &mut Child, path: &Path, needle: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if logs(path).contains(needle) {
                return;
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "server exited: {}",
                logs(path)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("missing {needle}: {}", logs(path)));
}

async fn exited(child: &mut Child, path: &Path, within: Duration) -> ExitStatus {
    tokio::time::timeout(within, child.wait())
        .await
        .unwrap_or_else(|_| panic!("shutdown exceeded {within:?}: {}", logs(path)))
        .unwrap()
}

async fn persists_after_signal(durable: bool, sig: &str) {
    let _guard = STACK_LOCK.lock().await;
    let Some(stack) = Stack::new().await else {
        return;
    };
    let (mut hub, log) = stack.hub("first", durable, &[]);
    wait_routing(&stack.url).await;
    let client = async_nats::connect(&stack.url).await.unwrap();
    let mut routed = client.subscribe("channel.shutdown.burst").await.unwrap();
    client.flush().await.unwrap();
    const N: usize = 1000;
    for i in 0..N {
        let env = Envelope::new(
            "shutdown-test",
            "shutdown.burst",
            MessageKind::Message,
            json!({"i": i, "body": "x".repeat(128)}),
        );
        client
            .publish(
                "hub.pub.shutdown-test.shutdown.burst",
                env.to_json_bytes().unwrap().into(),
            )
            .await
            .unwrap();
    }
    client.flush().await.unwrap();
    // Delivery proves the hub accepted each write, without waiting for the
    // asynchronous mirror to persist it. Signal immediately after delivery.
    tokio::time::timeout(Duration::from_secs(15), async {
        for _ in 0..N {
            routed.next().await.expect("routed envelope");
        }
    })
    .await
    .expect("burst routed");
    let started = Instant::now();
    signal(&hub, sig).await;
    let status = exited(&mut hub, &log, Duration::from_secs(12)).await;
    eprintln!("{sig}, durable={durable}: {status}");
    assert!(
        status.success(),
        "{sig}, durable={durable}: {status}; {}",
        logs(&log)
    );
    let output = logs(&log);
    assert!(output.contains("shutdown requested"), "{output}");
    assert!(output.contains("storage mirror drained"), "{output}");
    assert!(output.contains("shutdown complete"), "{output}");
    assert!(output.contains("storage closed"), "{output}");
    let engine_log = std::fs::read_to_string(stack.tmp.path().join("db/LOG")).unwrap();
    assert!(
        engine_log.contains("Manual flush start.")
            && engine_log.contains("\"event\": \"flush_finished\""),
        "datastore shutdown did not flush RocksDB memtables: {engine_log}"
    );
    println!(
        "{sig}, durable={durable}: {status}, {:?}\n{output}",
        started.elapsed()
    );

    let (mut restarted, restart_log) = stack.hub("restart", durable, &[]);
    wait_log(&mut restarted, &restart_log, "query API listening").await;
    let api = ApiClient::connect_with_identity(&stack.url, "test-admin")
        .await
        .unwrap();
    let history = api
        .request(
            "history.query",
            json!({"channel":"shutdown.burst", "limit":N}),
        )
        .await
        .unwrap();
    let rows = history["envelopes"].as_array().unwrap();
    assert_eq!(rows.len(), N, "history after immediate restart");
    let values: std::collections::BTreeSet<_> = rows
        .iter()
        .map(|r| r["payload"]["i"].as_u64().unwrap())
        .collect();
    assert_eq!(values, (0..N as u64).collect());
    println!("reopened same DB: {N}/{N} unique envelopes in history");
    signal(&restarted, "-TERM").await;
    assert!(
        exited(&mut restarted, &restart_log, Duration::from_secs(12))
            .await
            .success()
    );
}

#[tokio::test]
async fn sigterm_flushes_core_history_before_exit() {
    persists_after_signal(false, "-TERM").await;
}

#[tokio::test]
async fn sigterm_flushes_durable_history_before_exit() {
    persists_after_signal(true, "-TERM").await;
}

#[tokio::test]
async fn sigint_flushes_history_before_exit() {
    persists_after_signal(false, "-INT").await;
}

// Pass startup traffic normally, then withhold JetStream's destination
// publish acknowledgement. Gating before forwarding the publish makes the
// active durable handler deterministic, without relying on disk speed.
async fn stalled_publish_proxy(
    broker: String,
) -> (
    String,
    tokio_util::sync::CancellationToken,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("nats://{}", listener.local_addr().unwrap());
    let blocked = tokio_util::sync::CancellationToken::new();
    let gate = blocked.clone();
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                _ = connections.join_next(), if !connections.is_empty() => continue,
            };
            let (client, _) = accepted.unwrap();
            let broker = broker.clone();
            let gate = gate.clone();
            connections.spawn(async move {
                let server = tokio::net::TcpStream::connect(broker.trim_start_matches("nats://"))
                    .await
                    .unwrap();
                let (mut cr, mut cw) = client.into_split();
                let (mut sr, mut sw) = server.into_split();
                let upstream = async {
                    let mut buf = [0; 8192];
                    let mut tail = Vec::new();
                    loop {
                        let n = cr.read(&mut buf).await?;
                        if n == 0 {
                            return Ok::<_, std::io::Error>(());
                        }
                        tail.extend_from_slice(&buf[..n]);
                        if tail
                            .windows(b"channel.task.shutdown-stall".len())
                            .any(|w| w == b"channel.task.shutdown-stall")
                        {
                            gate.cancel();
                        }
                        sw.write_all(&buf[..n]).await?;
                        if tail.len() > 64 {
                            tail.drain(..tail.len() - 64);
                        }
                    }
                };
                let downstream = async {
                    tokio::select! {
                        biased;
                        _ = gate.cancelled() => std::future::pending::<()>().await,
                        _ = tokio::io::copy(&mut sr, &mut cw) => {},
                    }
                };
                tokio::select! { _ = upstream => {}, _ = downstream => {} }
            });
        }
    });
    (url, blocked, task)
}

async fn interrupted_shutdown(second_signal: bool) {
    let _guard = STACK_LOCK.lock().await;
    let Some(mut stack) = Stack::new().await else {
        return;
    };
    let broker = stack.url.clone();
    let (proxy_url, blocked, proxy) = stalled_publish_proxy(broker.clone()).await;
    stack.url = proxy_url;
    let extra = if second_signal {
        vec![]
    } else {
        vec!["--shutdown-timeout", "1"]
    };
    let (mut hub, log) = stack.hub("interrupted", true, &extra);
    wait_routing(&broker).await;
    let client = async_nats::connect(&broker).await.unwrap();
    let env = Envelope::new(
        "shutdown-test",
        "task.shutdown-stall",
        MessageKind::Message,
        json!({}),
    );
    client
        .publish(
            "hub.pub.shutdown-test.task.shutdown-stall",
            env.to_json_bytes().unwrap().into(),
        )
        .await
        .unwrap();
    client.flush().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), blocked.cancelled())
        .await
        .expect("durable republish reached proxy");
    signal(&hub, "-TERM").await;
    wait_log(&mut hub, &log, "shutdown requested").await;
    let started = Instant::now();
    if second_signal {
        signal(&hub, "-TERM").await;
    }
    let status = exited(&mut hub, &log, Duration::from_secs(3)).await;
    let output = logs(&log);
    assert!(!status.success(), "forced shutdown must fail: {output}");
    assert!(output.contains("shutdown forced"), "{output}");
    assert!(output.contains("mirror_writes_dropped"), "{output}");
    proxy.abort();
    println!(
        "forced shutdown (second_signal={second_signal}): {status}, {:?}\n{output}",
        started.elapsed()
    );
}

#[tokio::test]
async fn second_sigterm_exits_fast_and_nonzero() {
    interrupted_shutdown(true).await;
}

#[tokio::test]
async fn shutdown_timeout_exits_nonzero() {
    interrupted_shutdown(false).await;
}
