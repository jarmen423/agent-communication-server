//! Integration tests: inbox routing (meta.to), reply helpers, and
//! the HubClient send_to / send_reply / subscribe_inbox API.

use nats_hub::{Envelope, HubClient, MessageKind};
use serde_json::json;
use std::time::Duration;

async fn setup_nats() -> HubClient {
    // These tests require a running NATS server.
    // They're integration tests (not unit tests).
    let url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());

    // Try to connect — skip tests if NATS isn't running
    match HubClient::connect(&url, "test-runner").await {
        Ok(client) => client,
        Err(_) => {
            eprintln!("Skipping inbox routing tests — NATS server not available at {url}");
            std::process::exit(0);
        }
    }
}

#[tokio::test]
async fn test_send_to_routes_to_inbox() {
    let url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());

    // Skip if NATS not running
    if HubClient::connect(&url, "skip-check").await.is_err() {
        eprintln!("Skipping — NATS not available");
        return;
    }

    // Create receiver agent
    let receiver = HubClient::connect(&url, "test-receiver").await.unwrap();
    let mut inbox = receiver.subscribe_inbox().await.unwrap();

    // Give subscription a moment to register
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create sender and send a DM to the receiver
    let sender = HubClient::connect(&url, "test-sender").await.unwrap();
    let id = sender
        .send_to(
            "test-receiver",
            "test.channel",
            json!({"prompt": "hello from sender"}),
        )
        .await
        .unwrap();

    // Receiver should get it on their inbox
    let received = tokio::time::timeout(Duration::from_secs(2), inbox.recv())
        .await
        .expect("timeout waiting for inbox message")
        .expect("inbox stream closed");

    assert_eq!(received.meta.from, "test-sender");
    assert_eq!(received.meta.to, Some("test-receiver".to_string()));
    assert_eq!(received.payload["prompt"], "hello from sender");

    let _ = sender.drain().await;
    let _ = receiver.drain().await;
}

#[tokio::test]
async fn test_send_reply_correlation() {
    let url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());

    if HubClient::connect(&url, "skip-check").await.is_err() {
        eprintln!("Skipping — NATS not available");
        return;
    }

    // Agent A sends a task to Agent B
    let agent_a = HubClient::connect(&url, "agent-a").await.unwrap();
    let agent_b = HubClient::connect(&url, "agent-b").await.unwrap();

    let mut b_inbox = agent_b.subscribe_inbox().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // A sends task to B
    let task_id = agent_a
        .send_to("agent-b", "task.channel", json!({"prompt": "do work"}))
        .await
        .unwrap();

    // B receives and replies
    let task_env = tokio::time::timeout(Duration::from_secs(2), b_inbox.recv())
        .await
        .expect("timeout")
        .expect("stream closed");

    assert_eq!(task_env.meta.id, task_id);

    // B replies to A
    let reply_id = agent_b
        .send_reply(&task_env, json!({"result": "work done"}))
        .await
        .unwrap();

    // A receives the reply on their inbox
    let mut a_inbox = agent_a.subscribe_inbox().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let reply = tokio::time::timeout(Duration::from_secs(2), a_inbox.recv())
        .await
        .expect("timeout waiting for reply")
        .expect("stream closed");

    assert_eq!(reply.meta.id, reply_id);
    assert_eq!(reply.meta.from, "agent-b");
    assert_eq!(reply.meta.to, Some("agent-a".to_string()));
    assert_eq!(reply.meta.reply_to, Some(task_id));
    assert_eq!(reply.payload["result"], "work done");

    let _ = agent_a.drain().await;
    let _ = agent_b.drain().await;
}

#[tokio::test]
async fn test_inbox_subject_format() {
    // Test that the subjects::inbox helper produces the right format
    let subject = nats_hub::protocol::subjects::inbox("worker-1");
    assert_eq!(subject, "channel.inbox.worker-1");
}
