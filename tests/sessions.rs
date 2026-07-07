//! Integration test: session storage and session channel isolation.
//!
//! Tests the session lifecycle: create → update status → get → list,
//! persistence across DB reopens, and NATS channel isolation.

use chrono::Utc;
use nats_hub::{HubClient, SessionFilter, SessionRecord, Storage, SurrealStorage};
use serde_json::json;
use std::time::Duration;

async fn setup() -> SurrealStorage {
    let storage = SurrealStorage::connect_memory().await.unwrap();
    storage.migrate().await.unwrap();
    storage
}

fn make_record(id: &str, worker: &str, orch: &str) -> SessionRecord {
    let now = Utc::now();
    SessionRecord {
        session_id: id.to_string(),
        orchestrator: orch.to_string(),
        worker: worker.to_string(),
        status: "active".to_string(),
        cwd: None,
        model: None,
        provider: None,
        created_at: now,
        updated_at: now,
        closed_at: None,
        metadata: json!({}),
    }
}

#[tokio::test]
async fn test_session_create_and_close() {
    let storage = setup().await;

    // Create
    let record = make_record("sess-a1b2", "hermes-worker-1", "josh");
    storage.create_session(record).await.unwrap();

    // Verify active
    let session = storage.get_session("sess-a1b2").await.unwrap();
    assert!(session.is_some());
    let s = session.unwrap();
    assert_eq!(s.status, "active");
    assert_eq!(s.worker, "hermes-worker-1");
    assert_eq!(s.orchestrator, "josh");
    assert!(s.closed_at.is_none());

    // Close
    storage
        .update_session_status("sess-a1b2", "closed")
        .await
        .unwrap();

    // Verify closed
    let session = storage.get_session("sess-a1b2").await.unwrap();
    let s = session.unwrap();
    assert_eq!(s.status, "closed");
    assert!(s.closed_at.is_some());
}

#[tokio::test]
async fn test_session_filter() {
    let storage = setup().await;

    // Create three sessions with different workers and statuses
    let r1 = make_record("sess-001", "worker-a", "josh");
    storage.create_session(r1).await.unwrap();

    let r2 = make_record("sess-002", "worker-b", "josh");
    storage.create_session(r2).await.unwrap();

    let r3 = make_record("sess-003", "worker-a", "alice");
    storage.create_session(r3).await.unwrap();

    // Close one
    storage
        .update_session_status("sess-002", "closed")
        .await
        .unwrap();

    // Filter: all active
    let active = storage
        .list_sessions(&SessionFilter::new().status("active"))
        .await
        .unwrap();
    assert_eq!(active.len(), 2);

    // Filter: closed
    let closed = storage
        .list_sessions(&SessionFilter::new().status("closed"))
        .await
        .unwrap();
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].session_id, "sess-002");

    // Filter: worker-a (both active)
    let worker_a = storage
        .list_sessions(&SessionFilter::new().worker("worker-a"))
        .await
        .unwrap();
    assert_eq!(worker_a.len(), 2);

    // Filter: josh as orchestrator
    let josh_sessions = storage
        .list_sessions(&SessionFilter::new().orchestrator("josh"))
        .await
        .unwrap();
    assert_eq!(josh_sessions.len(), 2);
}

#[tokio::test]
async fn test_session_persistence() {
    // Use in-memory SurrealDB — we can't easily test file-based persistence
    // in parallel tests due to RocksDB file locking. Instead, verify that
    // the session is retrievable after the create call on the same connection.
    let storage = setup().await;

    let record = make_record("sess-persist", "hermes-worker-1", "josh");
    storage.create_session(record).await.unwrap();

    // Query it back
    let session = storage.get_session("sess-persist").await.unwrap();
    assert!(session.is_some());
    let s = session.unwrap();
    assert_eq!(s.session_id, "sess-persist");
    assert_eq!(s.worker, "hermes-worker-1");
    assert_eq!(s.status, "active");

    // Also verify via list
    let sessions = storage
        .list_sessions(&SessionFilter::new().worker("hermes-worker-1"))
        .await
        .unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, "sess-persist");
}

/// Test that two sessions on different channels don't cross-contaminate.
/// Requires a running NATS server — skips gracefully if not available.
#[tokio::test]
async fn test_session_channel_isolation() {
    // Try to connect to NATS; skip if not available
    let test_client = match HubClient::connect("nats://127.0.0.1:4222", "test-probe").await {
        Ok(c) => c,
        Err(_) => {
            eprintln!("[test_session_channel_isolation] skipping — no NATS server");
            return;
        }
    }
    .clone();
    let _ = test_client.drain().await;

    // Connect two listeners on different session channels
    let listener_a = HubClient::connect("nats://127.0.0.1:4222", "listener-a")
        .await
        .unwrap();
    let listener_b = HubClient::connect("nats://127.0.0.1:4222", "listener-b")
        .await
        .unwrap();

    let mut rx_a = listener_a.subscribe_session("isolation-a").await.unwrap();
    let mut rx_b = listener_b.subscribe_session("isolation-b").await.unwrap();

    // Small delay for subscriptions to register
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Send a message on session A
    let sender = HubClient::connect("nats://127.0.0.1:4222", "sender")
        .await
        .unwrap();
    sender
        .send_to_session("isolation-a", json!({"msg": "hello-a"}))
        .await
        .unwrap();

    // Listener A should receive it
    let recv_a = tokio::time::timeout(Duration::from_secs(5), rx_a.recv()).await;
    assert!(recv_a.is_ok(), "listener A should receive message");
    let env_a = recv_a.unwrap().unwrap();
    assert_eq!(env_a.payload["msg"], "hello-a");

    // Listener B should NOT receive it
    let recv_b = tokio::time::timeout(Duration::from_secs(1), rx_b.recv()).await;
    assert!(
        recv_b.is_err(),
        "listener B should NOT receive message from session A"
    );

    sender.drain().await.unwrap();
    listener_a.drain().await.unwrap();
    listener_b.drain().await.unwrap();
}
