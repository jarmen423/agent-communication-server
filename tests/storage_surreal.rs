//! Integration test: SurrealDB storage backend.
//!
//! Tests the full lifecycle: migrate → store envelope → query history →
//! store agent → find agent → link reply → get thread.

use chrono::Utc;
use nats_hub::{
    AgentFilter, AgentRecord, Envelope, EnvelopeRecord, HistoryQuery, MessageKind, Storage,
    SurrealStorage,
};
use serde_json::json;

async fn setup() -> SurrealStorage {
    let storage = SurrealStorage::connect_memory().await.unwrap();
    storage.migrate().await.unwrap();
    storage
}

#[tokio::test]
async fn test_store_and_query_envelope() {
    let storage = setup().await;

    let env = Envelope::new(
        "agent-alpha",
        "agents.broadcast",
        MessageKind::Message,
        json!({"text": "hello world"}),
    );

    storage.store_envelope(&env).await.unwrap();

    // Give the async store a moment
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let results = storage
        .query_history(&HistoryQuery::new().channel("agents.broadcast").limit(10))
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].from_identity, "agent-alpha");
    assert_eq!(results[0].channel, "agents.broadcast");
    assert_eq!(results[0].payload["text"], "hello world");
}

#[tokio::test]
async fn test_get_envelope_by_id() {
    let storage = setup().await;

    let env = Envelope::new(
        "agent-beta",
        "agents.tasks",
        MessageKind::Message,
        json!({"task": "compute"}),
    );

    let env_id = env.meta.id.clone();
    storage.store_envelope(&env).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let result = storage.get_envelope(&env_id).await.unwrap();
    assert!(result.is_some());
    let record = result.unwrap();
    assert_eq!(record.from_identity, "agent-beta");
    assert_eq!(record.payload["task"], "compute");
}

#[tokio::test]
async fn test_agent_registry() {
    let storage = setup().await;

    let agent = AgentRecord {
        identity: "agent-gamma".to_string(),
        capabilities: vec!["compute".into(), "observe".into()],
        last_seen: Utc::now(),
        registered_at: Utc::now(),
        metadata: json!({}),
    };

    storage.register_agent(agent).await.unwrap();

    let found = storage.get_agent("agent-gamma").await.unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().capabilities, vec!["compute", "observe"]);

    let agents = storage
        .find_agents(&AgentFilter::new().capabilities(vec!["compute".into()]))
        .await
        .unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].identity, "agent-gamma");
}

#[tokio::test]
async fn test_link_reply_graph_edge() {
    let storage = setup().await;

    let root = Envelope::new(
        "sender",
        "agents.tasks",
        MessageKind::Message,
        json!({"prompt": "compute"}),
    )
    .to("worker-1");
    let root_id = root.meta.id.clone();
    storage.store_envelope(&root).await.unwrap();

    let reply = Envelope::new(
        "worker-1",
        "agents.tasks",
        MessageKind::Message,
        json!({"result": "42"}),
    )
    .to("sender")
    .reply_to(&root_id);
    let reply_id = reply.meta.id.clone();
    storage.store_envelope(&reply).await.unwrap();

    // store_envelope calls link_reply; explicit call must also succeed (idempotent).
    storage
        .link_reply(&reply_id, &root_id)
        .await
        .expect("link_reply should succeed without SurrealQL parse errors");

    // Graph edge direction: reply(in) -> parent(out). Pending clears for recipient.
    let pending = storage.list_pending("worker-1").await.unwrap();
    assert_eq!(pending.len(), 0, "replied message should not be pending");
}

#[tokio::test]
async fn test_conversation_thread() {
    let storage = setup().await;

    // Root message
    let root = Envelope::new(
        "agent-alpha",
        "agents.chat",
        MessageKind::Message,
        json!({"text": "what is 2+2?"}),
    );
    let root_id = root.meta.id.clone();
    storage.store_envelope(&root).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Reply message
    let reply = Envelope::new(
        "agent-beta",
        "agents.chat",
        MessageKind::Message,
        json!({"text": "4"}),
    )
    .reply_to(&root_id);
    storage.store_envelope(&reply).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Get the thread
    let thread = storage.get_thread(&root_id).await.unwrap();
    assert_eq!(thread.len(), 2);
}

#[tokio::test]
async fn test_ping() {
    let storage = setup().await;
    storage.ping().await.unwrap();
}
