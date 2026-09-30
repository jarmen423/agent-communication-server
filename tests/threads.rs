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

fn msg(from: &str, text: &str) -> Envelope {
    Envelope::new(
        from,
        "agents.chat",
        MessageKind::Message,
        json!({ "text": text }),
    )
}

/// Store a chain root ← r1 ← r2 ← … (each replying to the previous one).
async fn store_chain(storage: &SurrealStorage, len: usize) -> Vec<String> {
    let root = msg("a", "root");
    let mut ids = vec![root.meta.id.clone()];
    storage.store_envelope(&root).await.unwrap();
    for i in 1..len {
        let reply =
            msg(if i % 2 == 0 { "a" } else { "b" }, &format!("r{i}")).reply_to(ids.last().unwrap());
        ids.push(reply.meta.id.clone());
        storage.store_envelope(&reply).await.unwrap();
    }
    ids
}

#[tokio::test]
async fn test_thread_follows_multi_level_chain() {
    let storage = setup().await;
    // root ← r1 ← r2 ← r3, plus a sibling branch root ← s1 ← s2.
    let ids = store_chain(&storage, 4).await;
    let s1 = msg("c", "s1").reply_to(&ids[0]);
    let s2 = msg("a", "s2").reply_to(&s1.meta.id);
    storage.store_envelope(&s1).await.unwrap();
    storage.store_envelope(&s2).await.unwrap();
    // Unrelated message must not leak in.
    storage.store_envelope(&msg("z", "noise")).await.unwrap();

    let thread = storage.get_thread(&ids[0]).await.unwrap();
    assert_eq!(thread.len(), 6, "root + 3 chain replies + 2 branch replies");
    assert!(thread[0].id.contains(&ids[0]), "root comes first");
    for id in ids.iter().chain([&s1.meta.id, &s2.meta.id]) {
        assert!(
            thread.iter().any(|r| r.id.contains(id.as_str())),
            "missing {id}"
        );
    }
    assert!(thread.iter().all(|r| r.payload["text"] != "noise"));

    // Starting mid-chain returns that subtree only.
    let sub = storage.get_thread(&ids[2]).await.unwrap();
    assert_eq!(sub.len(), 2);
}

#[tokio::test]
async fn test_thread_depth_cap_and_limit() {
    let storage = setup().await;
    let ids = store_chain(&storage, 6).await; // root + 5 levels

    let two_levels = storage.get_thread_bounded(&ids[0], 2, 100).await.unwrap();
    assert_eq!(two_levels.len(), 3, "root + 2 reply levels");

    let limited = storage.get_thread_bounded(&ids[0], 64, 4).await.unwrap();
    assert_eq!(limited.len(), 4);
    assert!(limited[0].id.contains(&ids[0]));

    let full = storage.get_thread(&ids[0]).await.unwrap();
    assert_eq!(full.len(), 6);
}

#[tokio::test]
async fn test_pending_ignores_progress_and_counts_message_replies() {
    let storage = setup().await;

    // Task delegated to worker-p (reply-contract shape).
    let task = Envelope::new(
        "orch",
        "tasks",
        MessageKind::Message,
        json!({"prompt": "work", "task_channel": "task.1"}),
    )
    .to("worker-p");
    storage.store_envelope(&task).await.unwrap();

    // Progress on the task channel is not an answer.
    for kind in [MessageKind::Status, MessageKind::Event] {
        let progress = Envelope::new("worker-p", "task.1", kind, json!({"status": "working"}))
            .reply_to(&task.meta.id);
        storage.store_envelope(&progress).await.unwrap();
    }
    let pending = storage.list_pending("worker-p").await.unwrap();
    assert_eq!(
        pending.len(),
        1,
        "status/event replies must not clear pending"
    );

    // The terminal result (kind=message, broadcast on the task channel) is.
    let result = Envelope::new(
        "worker-p",
        "task.1",
        MessageKind::Message,
        json!({"status": "done", "task_id": task.meta.id, "result": "ok", "error": null}),
    )
    .reply_to(&task.meta.id);
    storage.store_envelope(&result).await.unwrap();
    assert!(storage.list_pending("worker-p").await.unwrap().is_empty());

    // Status DMs addressed to an agent are not "pending" asks.
    let status_dm = Envelope::new("orch", "tasks", MessageKind::Status, json!({})).to("worker-p");
    storage.store_envelope(&status_dm).await.unwrap();
    assert!(storage.list_pending("worker-p").await.unwrap().is_empty());
}

#[tokio::test]
async fn test_pending_bounded_newest_first() {
    let storage = setup().await;
    for i in 0..5 {
        let m = Envelope::new("s", "q", MessageKind::Message, json!({ "i": i })).to("busy");
        storage.store_envelope(&m).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    let all = storage.list_pending("busy").await.unwrap();
    assert_eq!(all.len(), 5);
    assert_eq!(all[0].payload["i"], 4, "newest first");

    let two = storage.list_pending_bounded("busy", 2).await.unwrap();
    assert_eq!(two.len(), 2);
    assert_eq!(two[0].payload["i"], 4);
    assert!(storage.list_pending("nobody").await.unwrap().is_empty());
}
