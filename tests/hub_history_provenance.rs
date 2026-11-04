//! `hub-history` provenance filters against the bound test stack
//! (`NATS_HUB_REQUIRE_BOUND=1 scripts/dev/with_stack.sh`, i.e. `make
//! test-bound`). The stack's `test-admin` is an `--api-admin`, so it may
//! `manifest.put` records even though no manifest writer is configured;
//! the router then stamps trusted provenance from that record.

use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

mod support;

fn bound_stack_url() -> Option<String> {
    if std::env::var("NATS_HUB_REQUIRE_BOUND").ok().as_deref() != Some("1") {
        eprintln!("Skipping — needs the bound stack (NATS_HUB_REQUIRE_BOUND=1)");
        return None;
    }
    Some(std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into()))
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

async fn api(nc: &async_nats::Client, caller: &str, op: &str, params: Value) -> Value {
    let body = serde_json::to_vec(&json!({"op": op, "params": params})).unwrap();
    let msg = tokio::time::timeout(
        Duration::from_secs(10),
        nc.request(format!("hub.api.{caller}.{op}"), body.into()),
    )
    .await
    .expect("no reply in time")
    .expect("request failed");
    serde_json::from_slice(&msg.payload).unwrap()
}

/// Poll `history.query` until the router's async mirror stored `id`.
async fn wait_stored(nc: &async_nats::Client, channel: &str, id: &str) {
    for _ in 0..100 {
        let resp = api(
            nc,
            "test-admin",
            "history.query",
            json!({"channel": channel, "limit": 5}),
        )
        .await;
        assert_eq!(resp["ok"], true, "{resp}");
        let found = resp["data"]["envelopes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"].as_str().is_some_and(|got| got.contains(id)));
        if found {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("envelope {id} was never stored");
}

fn hub_history(url: &str, extra: &[&str]) -> std::process::Output {
    Command::new(support::client_bin("hub-history"))
        .args(["--nats-url", url])
        .args(extra)
        .env("NATS_URL", url)
        .env("NATS_HUB_IDENTITY", "test-admin")
        .output()
        .expect("run hub-history")
}

#[tokio::test]
async fn hub_history_filters_on_provenance() {
    let Some(url) = bound_stack_url() else { return };
    let nc = async_nats::connect(&url).await.unwrap();
    let (sender, user, agent) = (unique("hist-a"), unique("alice"), unique("agentc"));
    let (channel, model, session) = (unique("hist"), unique("model"), unique("session"));
    let marker = unique("marker");

    let record = json!({
        "identity": sender, "owner_kind": "user", "user": user,
        "user_id": "6f9619ff-8b86-4011-b42d-00c04fc964ff", "device": "laptop",
        "agent": agent, "rev": chrono::Utc::now().timestamp_millis(),
    });
    let resp = api(&nc, "test-admin", "manifest.put", json!({"record": record})).await;
    assert_eq!(resp["ok"], true, "{resp}");

    let id = uuid::Uuid::new_v4().to_string();
    let env = json!({
        "meta": {
            "id": id, "from": sender, "channel": channel,
            "timestamp": "2026-01-01T00:00:00Z", "kind": "message",
            "origin": {"reported": {"model": model, "session": session}},
        },
        "payload": {"text": marker},
    });
    nc.publish(
        format!("hub.pub.{sender}.{channel}"),
        serde_json::to_vec(&env).unwrap().into(),
    )
    .await
    .unwrap();
    nc.flush().await.unwrap();
    wait_stored(&nc, &channel, &id).await;

    let expected_label = format!("{user} · {agent} · ~{model}");
    for (flag, value) in [
        ("--user", user.as_str()),
        ("--agent", agent.as_str()),
        ("--model", model.as_str()),
        ("--session", session.as_str()),
    ] {
        let out = hub_history(&url, &["--channel", &channel, flag, value]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{flag}: {out:?}");
        assert!(
            stdout.contains(&marker),
            "{flag} must list the message: {stdout}"
        );
        assert!(
            stdout.contains(&expected_label),
            "{flag} must show the provenance label: {stdout}"
        );
    }

    // A non-matching provenance value lists nothing.
    let out = hub_history(&url, &["--channel", &channel, "--user", "somebody-else"]);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("(no messages found)"));

    // --json prints the stored record unchanged, provenance included.
    let out = hub_history(&url, &["--channel", &channel, "--user", &user, "--json"]);
    assert!(out.status.success(), "{out:?}");
    let parsed: Value = serde_json::from_slice(&out.stdout).unwrap();
    let records = parsed.as_array().unwrap();
    assert_eq!(records.len(), 1, "{parsed}");
    assert_eq!(records[0]["origin"]["user"], user.as_str());
    assert_eq!(records[0]["origin"]["agent"], agent.as_str());
    assert_eq!(records[0]["origin"]["reported"]["model"], model.as_str());
    assert_eq!(
        records[0]["origin"]["reported"]["session"],
        session.as_str()
    );
}
