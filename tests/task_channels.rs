//! Integration tests: task channels and hub-delegate pattern.
//!
//! Tests that task channels provide isolated bidirectional conversation
//! and that multiple parallel tasks don't interfere with each other.

use nats_hub::{Envelope, HubClient, MessageKind};
use serde_json::json;
use std::time::Duration;

async fn try_connect(url: &str, identity: &str) -> Option<HubClient> {
    HubClient::connect(url, identity).await.ok()
}

/// Test that two task channels are isolated — messages on task.A
/// don't leak to task.B.
#[tokio::test]
async fn test_task_channel_isolation() {
    let url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());

    let sender = match try_connect(&url, "test-sender-isolation").await {
        Some(c) => c,
        None => {
            eprintln!("Skipping — NATS not available");
            return;
        }
    };

    // Subscribe to two separate task channels
    let mut rx_a = sender.subscribe_channel("task.isolation-a").await.unwrap();
    let mut rx_b = sender.subscribe_channel("task.isolation-b").await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Send a message only to task A
    sender
        .send_message("task.isolation-a", json!({"prompt": "task A"}))
        .await
        .unwrap();

    // Send a message only to task B
    sender
        .send_message("task.isolation-b", json!({"prompt": "task B"}))
        .await
        .unwrap();

    // rx_a should only get task A
    let msg_a = tokio::time::timeout(Duration::from_secs(2), rx_a.recv())
        .await
        .expect("timeout on channel A")
        .expect("channel A closed");
    assert_eq!(msg_a.payload["prompt"], "task A");

    // rx_b should only get task B
    let msg_b = tokio::time::timeout(Duration::from_secs(2), rx_b.recv())
        .await
        .expect("timeout on channel B")
        .expect("channel B closed");
    assert_eq!(msg_b.payload["prompt"], "task B");

    // Verify no cross-contamination: rx_a should NOT have task B
    let none_a = tokio::time::timeout(Duration::from_millis(500), rx_a.recv()).await;
    assert!(none_a.is_err(), "channel A received message from B!");

    // Verify no cross-contamination: rx_b should NOT have task A
    let none_b = tokio::time::timeout(Duration::from_millis(500), rx_b.recv()).await;
    assert!(none_b.is_err(), "channel B received message from A!");

    let _ = sender.drain().await;
}

/// Test the full delegate flow: send task with meta.to + task_channel,
/// receive reply on the task channel.
#[tokio::test]
async fn test_delegate_round_trip() {
    let url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());

    let orchestrator = match try_connect(&url, "test-orchestrator").await {
        Some(c) => c,
        None => {
            eprintln!("Skipping — NATS not available");
            return;
        }
    };
    let worker = try_connect(&url, "test-worker-delegate").await.unwrap();

    // Worker subscribes to its inbox
    let mut worker_inbox = worker.subscribe_inbox().await.unwrap();
    // Orchestrator subscribes to the task channel
    let task_channel = "task.delegate-test";
    let mut task_rx = orchestrator.subscribe_channel(task_channel).await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Orchestrator sends task to worker's inbox with task_channel in payload
    let task_id = orchestrator
        .send_to(
            "test-worker-delegate",
            task_channel,
            json!({
                "prompt": "echo test",
                "task_channel": task_channel,
            }),
        )
        .await
        .unwrap();

    // Worker receives the task on its inbox
    let task = tokio::time::timeout(Duration::from_secs(5), worker_inbox.recv())
        .await
        .expect("timeout waiting for task")
        .expect("inbox closed");

    assert_eq!(task.meta.id, task_id);
    assert_eq!(task.payload["prompt"], "echo test");
    assert_eq!(task.payload["task_channel"], task_channel);

    // Worker publishes result to the task channel (broadcast, no meta.to)
    worker
        .send_message(
            task_channel,
            json!({
                "result": "echo test done",
                "task_id": task_id,
                "status": "done",
            }),
        )
        .await
        .unwrap();

    // Orchestrator receives the result on the task channel
    let reply = tokio::time::timeout(Duration::from_secs(5), task_rx.recv())
        .await
        .expect("timeout waiting for reply")
        .expect("task channel closed");

    assert_eq!(reply.payload["result"], "echo test done");
    assert_eq!(reply.payload["status"], "done");

    let _ = orchestrator.drain().await;
    let _ = worker.drain().await;
}

/// Test that list_pending returns messages addressed to an agent
/// that haven't been replied to.
#[tokio::test]
async fn test_list_pending_storage() {
    use nats_hub::{HistoryQuery, Storage, SurrealStorage};

    let storage = SurrealStorage::connect_memory().await.unwrap();
    storage.migrate().await.unwrap();

    // Store an envelope addressed to "agent-x"
    let env = Envelope::new(
        "sender",
        "test.channel",
        MessageKind::Message,
        json!({"prompt": "do something"}),
    )
    .to("agent-x");
    storage.store_envelope(&env).await.unwrap();

    // List pending for agent-x
    let pending = storage.list_pending("agent-x").await.unwrap();
    assert_eq!(pending.len(), 1, "should have 1 pending message");
    assert_eq!(pending[0].to_identity, Some("agent-x".to_string()));
    assert_eq!(pending[0].payload["prompt"], "do something");

    // List pending for a different agent should be empty
    let pending_other = storage.list_pending("agent-y").await.unwrap();
    assert_eq!(
        pending_other.len(),
        0,
        "agent-y should have no pending messages"
    );

    // Now store a reply from agent-x
    let reply = Envelope::new(
        "agent-x",
        "test.channel",
        MessageKind::Message,
        json!({"result": "done"}),
    )
    .to("sender")
    .reply_to(&env.meta.id);
    storage.store_envelope(&reply).await.unwrap();

    let pending_after = storage.list_pending("agent-x").await.unwrap();
    assert_eq!(
        pending_after.len(),
        0,
        "original message should leave pending once reply edge exists"
    );
}
