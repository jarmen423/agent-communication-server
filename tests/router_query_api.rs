//! Query API (hub.api.*) dispatch over `Arc<dyn Storage>`, without NATS:
//! default/max limits on history, thread and pending responses.

use std::sync::Arc;

use nats_hub::query_api::{encode_response, handle_request, ApiResponse, DEFAULT_LIMIT, MAX_LIMIT};
use nats_hub::{Envelope, MessageKind, Storage, SurrealStorage};
use serde_json::{json, Value};

async fn storage() -> Arc<dyn Storage> {
    let s = SurrealStorage::connect_memory().await.unwrap();
    s.migrate().await.unwrap();
    Arc::new(s)
}

async fn call(s: &Arc<dyn Storage>, op: &str, params: Value) -> ApiResponse {
    let body = serde_json::to_vec(&json!({"op": op, "params": params})).unwrap();
    handle_request(s, &format!("hub.api.{op}"), &body).await
}

fn rows(resp: &ApiResponse, key: &str) -> usize {
    assert!(resp.ok, "expected ok, got {:?}", resp.error);
    resp.data.as_ref().unwrap()[key].as_array().unwrap().len()
}

async fn seed_dms(s: &Arc<dyn Storage>, to: &str, n: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(n);
    for i in 0..n {
        let e = Envelope::new("seed", "bulk", MessageKind::Message, json!({ "i": i })).to(to);
        ids.push(e.meta.id.clone());
        s.store_envelope(&e).await.unwrap();
    }
    ids
}

#[tokio::test]
async fn history_defaults_to_100_and_rejects_above_max() {
    let s = storage().await;
    seed_dms(&s, "bulk-agent", DEFAULT_LIMIT + 20).await;

    let resp = call(&s, "history.query", json!({"channel": "bulk"})).await;
    assert_eq!(rows(&resp, "envelopes"), DEFAULT_LIMIT);
    assert_eq!(resp.data.as_ref().unwrap()["limit"], DEFAULT_LIMIT);

    let resp = call(&s, "history.query", json!({"channel": "bulk", "limit": 7})).await;
    assert_eq!(rows(&resp, "envelopes"), 7);

    let resp = call(&s, "history.query", json!({"limit": MAX_LIMIT})).await;
    assert!(resp.ok);

    let resp = call(&s, "history.query", json!({"limit": MAX_LIMIT + 1})).await;
    assert!(!resp.ok);
    let err = resp.error.unwrap();
    assert!(err.contains("exceeds the maximum of 1000"), "{err}");

    let resp = call(&s, "history.query", json!({"limit": 0})).await;
    assert!(!resp.ok);
}

#[tokio::test]
async fn pending_is_bounded() {
    let s = storage().await;
    seed_dms(&s, "busy", DEFAULT_LIMIT + 5).await;

    let resp = call(&s, "thread.pending", json!({"identity": "busy"})).await;
    assert_eq!(rows(&resp, "pending"), DEFAULT_LIMIT);

    let resp = call(
        &s,
        "thread.pending",
        json!({"identity": "busy", "limit": 3}),
    )
    .await;
    assert_eq!(rows(&resp, "pending"), 3);

    let resp = call(
        &s,
        "thread.pending",
        json!({"identity": "busy", "limit": 5000}),
    )
    .await;
    assert!(!resp.ok);

    let resp = call(
        &s,
        "thread.pending",
        json!({"identity": "busy", "limit": "all"}),
    )
    .await;
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("positive integer"));
}

#[tokio::test]
async fn thread_is_bounded_and_deep() {
    let s = storage().await;
    // root ← r1 ← r2 ← r3
    let root = Envelope::new("a", "chat", MessageKind::Message, json!({}));
    let mut prev = root.meta.id.clone();
    s.store_envelope(&root).await.unwrap();
    for _ in 0..3 {
        let r = Envelope::new("b", "chat", MessageKind::Message, json!({})).reply_to(&prev);
        prev = r.meta.id.clone();
        s.store_envelope(&r).await.unwrap();
    }

    let resp = call(&s, "thread.get", json!({"root_id": root.meta.id})).await;
    assert_eq!(rows(&resp, "thread"), 4);

    let resp = call(
        &s,
        "thread.get",
        json!({"root_id": root.meta.id, "limit": 2}),
    )
    .await;
    assert_eq!(rows(&resp, "thread"), 2);

    let resp = call(
        &s,
        "thread.get",
        json!({"root_id": root.meta.id, "max_depth": 1}),
    )
    .await;
    assert_eq!(rows(&resp, "thread"), 2);

    let resp = call(
        &s,
        "thread.get",
        json!({"root_id": root.meta.id, "limit": 1001}),
    )
    .await;
    assert!(!resp.ok);

    let resp = call(&s, "thread.get", json!({})).await;
    assert_eq!(resp.error.as_deref(), Some("missing root_id"));
}

#[tokio::test]
async fn other_ops_still_dispatch_through_dyn_storage() {
    let s = storage().await;
    assert!(call(&s, "ping", json!({})).await.ok);
    assert!(call(&s, "wave.list", json!({})).await.ok);
    assert!(call(&s, "agent.find", json!({})).await.ok);
    let stats = call(&s, "stats.message_rate", json!({"secs": 60})).await;
    assert!(stats.ok, "{:?}", stats.error);
    let unknown = call(&s, "nope", json!({})).await;
    assert!(unknown.error.unwrap().contains("unknown operation"));
}

#[test]
fn oversized_responses_become_errors() {
    let big = ApiResponse::ok(json!({"blob": "x".repeat(2048)}));
    let bytes = encode_response(&big, 1024);
    let decoded: ApiResponse = serde_json::from_slice(&bytes).unwrap();
    assert!(!decoded.ok);
    assert!(decoded.error.unwrap().contains("max_payload"));

    let small = ApiResponse::ok(json!({"a": 1}));
    let decoded: ApiResponse = serde_json::from_slice(&encode_response(&small, 1024)).unwrap();
    assert!(decoded.ok);
}
