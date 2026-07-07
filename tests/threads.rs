//! Integration tests for conversation thread queries used by hub-thread.

use nats_hub::{Envelope, MessageKind, Storage, SurrealStorage};
use serde_json::json;

async fn setup() -> SurrealStorage {
    let storage = SurrealStorage::connect_memory().await.unwrap();
    storage.migrate().await.unwrap();
    storage
}

#[tokio::test]
async fn test_thread_show_root_and_reply() {
    let storage = setup().await;

    let root = Envelope::new(
        "agent-alpha",
        "agents.chat",
        MessageKind::Message,
        json!({"text": "what is 2+2?"}),
    );
    let root_id = root.meta.id.clone();
    storage.store_envelope(&root).await.unwrap();

    let reply = Envelope::new(
        "agent-beta",
        "agents.chat",
        MessageKind::Message,
        json!({"text": "4"}),
    )
    .reply_to(&root_id);
    storage.store_envelope(&reply).await.unwrap();

    let thread = storage.get_thread(&root_id).await.unwrap();
    assert_eq!(thread.len(), 2);
    assert!(thread.iter().any(|r| r.from_identity == "agent-alpha"));
    assert!(thread
        .iter()
        .any(|r| r.reply_to.as_deref() == Some(root_id.as_str())));
}

#[tokio::test]
async fn test_list_pending_for_agent() {
    let storage = setup().await;

    let msg = Envelope::new(
        "sender",
        "tasks",
        MessageKind::Message,
        json!({"prompt": "do work"}),
    )
    .to("worker-1");
    storage.store_envelope(&msg).await.unwrap();

    let pending = storage.list_pending("worker-1").await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].from_identity, "sender");
}
