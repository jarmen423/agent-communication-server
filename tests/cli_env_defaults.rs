//! F2 (first-run wave): the `hub-*` CLIs take `NATS_URL` and
//! `NATS_HUB_IDENTITY` as defaults, so `acs exec <id> -- hub-…` needs no
//! flags, and a missing identity is named instead of timing out.
//!
//! Every child gets `NATS_URL` from its environment and no `--nats-url`.
//! Live parts need `scripts/dev/with_stack.sh` (`NATS_HUB_TEST_STACK=1`).

mod support;

use nats_hub::{Envelope, HubClient};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

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

/// `bin` with only env defaults: `NATS_URL` set, `NATS_HUB_IDENTITY` =
/// `identity` (or removed).
fn cli(bin: &str, url: &str, identity: Option<&str>) -> Command {
    let mut cmd = Command::new(support::client_bin(bin));
    cmd.env("NATS_URL", url)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    match identity {
        Some(id) => cmd.env("NATS_HUB_IDENTITY", id),
        None => cmd.env_remove("NATS_HUB_IDENTITY"),
    };
    cmd
}

#[tokio::test]
async fn hub_publish_takes_url_and_sender_from_env() {
    let Some(url) = stack_url() else { return };
    let channel = uniq("env-defaults");
    let sender = uniq("env-sender");
    let watcher = HubClient::connect(&url, uniq("watch")).await.unwrap();
    let mut rx = watcher.subscribe_channel(&channel).await.unwrap();

    let out = cli("hub-publish", &url, Some(&sender))
        .args(["--channel", &channel, "--message", "hi"])
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let env: Envelope = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("no envelope within 10s")
        .expect("subscription closed");
    assert_eq!(env.meta.from, sender);
    assert_eq!(env.payload["text"], "hi");
}

#[tokio::test]
async fn missing_sender_identity_is_named() {
    let Some(url) = stack_url() else { return };
    for (bin, args) in [
        ("hub-publish", vec!["--channel", "c", "--message", "m"]),
        ("hub-worker", vec!["--execute", "cat"]),
        ("hub-session", vec!["send", "s1", "--message", "m"]),
    ] {
        let out = cli(bin, &url, None).args(&args).output().await.unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{bin} ran without an identity");
        assert!(
            stderr.contains("NATS_HUB_IDENTITY") && stderr.contains("acs exec"),
            "{bin}: {stderr}"
        );
    }
}

#[tokio::test]
async fn delegate_round_trip_with_env_defaults_only() {
    let Some(url) = stack_url() else { return };
    let worker = uniq("env-worker");

    // The worker's identity also comes from NATS_HUB_IDENTITY.
    let probe = HubClient::connect(&url, uniq("probe")).await.unwrap();
    let mut reg = probe.subscribe_subject("hub.register.*").await.unwrap();
    let _worker = cli("hub-worker", &url, Some(&worker))
        .args(["--execute", "cat"])
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn hub-worker");
    let registered = async {
        while let Some(env) = reg.recv().await {
            if env.payload["identity"] == worker.as_str() {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(20), registered)
        .await
        .expect("worker did not register");

    let out = cli("hub-delegate", &url, Some(&uniq("env-orch")))
        .args(["--to", &worker, "--prompt", "hello env", "--timeout", "30"])
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello env");
}
