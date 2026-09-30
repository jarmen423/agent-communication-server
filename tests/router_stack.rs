//! End-to-end checks against the live hub-server that
//! `scripts/dev/with_stack.sh` starts (router + SurrealDB + query API).
//! Skipped unless `NATS_HUB_TEST_STACK=1` (set by `with_stack.sh`).

use std::time::Duration;

use nats_hub::{ApiClient, HubClient};
use serde_json::{json, Value};

fn stack_url() -> Option<String> {
    if std::env::var("NATS_HUB_TEST_STACK").ok().as_deref() != Some("1") {
        eprintln!("Skipping — needs scripts/dev/with_stack.sh (NATS_HUB_TEST_STACK=1)");
        return None;
    }
    Some(std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into()))
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// Poll a query-API op until `ready` accepts the response (the storage
/// mirror is asynchronous).
async fn poll(api: &ApiClient, op: &str, params: Value, ready: impl Fn(&Value) -> bool) -> Value {
    let mut last = Value::Null;
    for _ in 0..50 {
        if let Ok(v) = api.request(op, params.clone()).await {
            if ready(&v) {
                return v;
            }
            last = v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{op} never became ready; last response: {last}");
}

#[tokio::test]
async fn heartbeat_keeps_persisted_capabilities() {
    let Some(url) = stack_url() else { return };
    let ident = unique("l2-hb");
    let agent = HubClient::connect(&url, ident.as_str()).await.unwrap();
    let api = ApiClient::connect(&url).await.unwrap();

    agent
        .register(vec!["code".into(), "review".into()])
        .await
        .unwrap();
    poll(&api, "agent.get", json!({"identity": ident}), |v| {
        !v["agent"].is_null()
    })
    .await;
    let before = api
        .request("agent.get", json!({"identity": ident}))
        .await
        .unwrap();

    agent.heartbeat().await.unwrap();
    let after = poll(&api, "agent.get", json!({"identity": ident}), |v| {
        v["agent"]["last_seen"] != before["agent"]["last_seen"]
    })
    .await;
    assert_eq!(after["agent"]["capabilities"], json!(["code", "review"]));
    let _ = agent.drain().await;
}

#[tokio::test]
async fn routed_dm_is_delivered_and_persisted() {
    let Some(url) = stack_url() else { return };
    let to = unique("l2-rcv");
    let receiver = HubClient::connect(&url, to.as_str()).await.unwrap();
    let mut inbox = receiver.subscribe_inbox().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let sender = HubClient::connect(&url, "l2-sender").await.unwrap();
    let channel = unique("l2.chan");
    let id = sender
        .send_to(&to, &channel, json!({"n": 1}))
        .await
        .unwrap();

    let got = tokio::time::timeout(Duration::from_secs(3), inbox.recv())
        .await
        .expect("DM not routed")
        .unwrap();
    assert_eq!(got.meta.id, id);

    let api = ApiClient::connect(&url).await.unwrap();
    let hist = poll(&api, "history.query", json!({"channel": channel}), |v| {
        v["envelopes"].as_array().is_some_and(|a| !a.is_empty())
    })
    .await;
    assert_eq!(hist["envelopes"][0]["to_identity"], json!(to));
    let pending = api
        .request("thread.pending", json!({"identity": to}))
        .await
        .unwrap();
    assert_eq!(pending["pending"].as_array().unwrap().len(), 1);

    // Over-limit requests fail with a clear error instead of a huge reply.
    let err = api
        .request("history.query", json!({"limit": 100_000}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("exceeds the maximum"), "{err}");

    let _ = sender.drain().await;
    let _ = receiver.drain().await;
}
