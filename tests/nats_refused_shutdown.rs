//! A broker that rejects authentication must not hold up storage shutdown.
#![cfg(all(unix, feature = "storage-surreal"))]
mod delivery_support;

use delivery_support::{free_port, hub_server_bin, nats_server_bin, STACK_LOCK};
use futures_util::StreamExt;
use nats_hub::{Envelope, MessageKind};
use serde_json::json;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::process::{Child, Command};

async fn signal(child: &Child, name: &str) {
    assert!(Command::new("kill")
        .args([name, &child.id().unwrap().to_string()])
        .status()
        .await
        .unwrap()
        .success());
}

async fn wait_log(child: &mut Child, log: &Path, needle: &str) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let text = std::fs::read_to_string(log).unwrap();
            if text.contains(needle) {
                return;
            }
            assert!(child.try_wait().unwrap().is_none(), "hub exited: {text}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "missing {needle}: {}",
            std::fs::read_to_string(log).unwrap()
        )
    });
}

async fn refused_shutdown(durable: bool) {
    if std::env::var_os("NATS_URL").is_none() {
        eprintln!("Skipping: run via scripts/dev/with_stack.sh");
        return;
    }
    let _guard = STACK_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let port = free_port();
    let config = tmp.path().join("nats.conf");
    let write_config = |password: &str| {
        std::fs::write(&config, format!(
            "host: 127.0.0.1\nport: {port}\njetstream {{ store_dir: {:?} }}\nauthorization {{ users: [{{ user: hub-server, password: {password} }}] }}\n",
            tmp.path().join("js"))).unwrap();
    };
    write_config("before");
    let nats_log = tmp.path().join("nats.log");
    let mut broker = Command::new(nats_server_bin().expect("make setup"))
        .arg("-c")
        .arg(&config)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&nats_log).unwrap())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let url = format!("nats://127.0.0.1:{port}");
    let hub_log = tmp.path().join("hub.log");
    let spawn_hub = || {
        let log = std::fs::File::create(&hub_log).unwrap();
        let mut command = Command::new(hub_server_bin());
        command
            .args(["--nats-url", &url, "--db-path"])
            .arg(tmp.path().join("db"))
            .env_remove("NATS_CREDENTIALS_FILE")
            .env_remove("NATS_TOKEN")
            .env_remove("NATS_NKEY")
            .env_remove("NATS_HUB_IDENTITY")
            .env("NATS_USER", "hub-server")
            .env("NATS_PASSWORD", "before")
            .env("RUST_LOG", "nats_hub=info,async_nats=info")
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true);
        if durable {
            command.arg("--durable");
        }
        command.spawn().unwrap()
    };
    let mut hub = spawn_hub();
    wait_log(
        &mut hub,
        &hub_log,
        if durable {
            "routing from the durable ingress"
        } else {
            "control plane subscribed"
        },
    )
    .await;
    let nc =
        async_nats::ConnectOptions::with_user_and_password("hub-server".into(), "before".into())
            .connect(&url)
            .await
            .unwrap();
    let mut routed = nc.subscribe("channel.task.refused-shutdown").await.unwrap();
    nc.flush().await.unwrap();
    let envelope = Envelope::new(
        "test",
        "task.refused-shutdown",
        MessageKind::Message,
        json!({"kept":true}),
    );
    nc.publish(
        "hub.pub.test.task.refused-shutdown",
        envelope.to_json_bytes().unwrap().into(),
    )
    .await
    .unwrap();
    nc.flush().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), routed.next())
        .await
        .unwrap()
        .unwrap();
    write_config("refused");
    signal(&broker, "-HUP").await;
    wait_log(&mut broker, &nats_log, "Reloaded server configuration").await;
    wait_log(&mut hub, &hub_log, "disconnected").await;
    let started = std::time::Instant::now();
    signal(&hub, "-TERM").await;
    let status = tokio::time::timeout(Duration::from_secs(3), hub.wait())
        .await
        .unwrap_or_else(|_| {
            println!("FAIL: refused broker held shutdown beyond 3 seconds");
            panic!(
                "refused broker held shutdown: {}",
                std::fs::read_to_string(&hub_log).unwrap()
            )
        })
        .unwrap();
    let output = std::fs::read_to_string(&hub_log).unwrap();
    assert!(status.success(), "{status}: {output}");
    for message in [
        "storage mirror drained",
        "storage closed",
        "shutdown complete",
    ] {
        assert!(output.contains(message), "missing {message}: {output}");
    }
    println!(
        "PASS refused NATS shutdown durable={durable}: {status}, {:?}\n{output}",
        started.elapsed()
    );

    // Reopen the exact DB after restoring NATS, and prove the accepted write survived.
    write_config("before");
    signal(&broker, "-HUP").await;
    let mut hub = spawn_hub();
    wait_log(
        &mut hub,
        &hub_log,
        if durable {
            "routing from the durable ingress"
        } else {
            "control plane subscribed"
        },
    )
    .await;
    let nc =
        async_nats::ConnectOptions::with_user_and_password("hub-server".into(), "before".into())
            .connect(&url)
            .await
            .unwrap();
    let reply = nc
        .request(
            "hub.api.history.query",
            json!({"op":"history.query", "params":{"channel":"task.refused-shutdown"}})
                .to_string()
                .into(),
        )
        .await
        .unwrap();
    let response: serde_json::Value = serde_json::from_slice(&reply.payload).unwrap();
    assert_eq!(
        response["data"]["envelopes"].as_array().unwrap().len(),
        1,
        "{response}"
    );
    assert_eq!(response["data"]["envelopes"][0]["payload"]["kept"], true);
    println!("PASS reopened DB after refused NATS shutdown: accepted envelope retained (durable={durable})");
    signal(&hub, "-TERM").await;
    assert!(tokio::time::timeout(Duration::from_secs(12), hub.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
}

#[tokio::test]
async fn refused_nats_core_shutdown_flushes_storage_and_exits_zero() {
    refused_shutdown(false).await;
}

#[tokio::test]
async fn refused_nats_durable_shutdown_flushes_storage_and_exits_zero() {
    refused_shutdown(true).await;
}
