//! Integration tests for the agent registry — both the in-memory cache
//! and the SurrealDB-backed `Storage` implementation.
//!
//! These tests cover Phase 2 of the persistence layer: capability filters,
//! liveness windows, touch semantics, and deregistration.

use chrono::Utc;
use nats_hub::client::AgentRegistry;
use nats_hub::{AgentFilter, AgentRecord, Storage, SurrealStorage};
use serde_json::json;
use std::time::Duration;

// ── SurrealDB Storage tests ─────────────────────────────────

async fn setup_storage() -> SurrealStorage {
    let storage = SurrealStorage::connect_memory().await.unwrap();
    storage.migrate().await.unwrap();
    storage
}

fn record(identity: &str, caps: &[&str]) -> AgentRecord {
    AgentRecord {
        identity: identity.to_string(),
        capabilities: caps.iter().map(|s| s.to_string()).collect(),
        last_seen: Utc::now(),
        registered_at: Utc::now(),
        metadata: json!({}),
    }
}

#[tokio::test]
async fn test_find_agents_by_capability() {
    let storage = setup_storage().await;

    storage
        .register_agent(record("alpha", &["compute", "observe"]))
        .await
        .unwrap();
    storage
        .register_agent(record("beta", &["compute"]))
        .await
        .unwrap();
    storage
        .register_agent(record("gamma", &["observe"]))
        .await
        .unwrap();

    // Single capability filter
    let only_compute = storage
        .find_agents(&AgentFilter::new().capabilities(vec!["compute".into()]))
        .await
        .unwrap();
    let ids: Vec<&str> = only_compute.iter().map(|a| a.identity.as_str()).collect();
    assert_eq!(only_compute.len(), 2);
    assert!(ids.contains(&"alpha"));
    assert!(ids.contains(&"beta"));

    // Both capabilities (AND)
    let both = storage
        .find_agents(&AgentFilter::new().capabilities(vec!["compute".into(), "observe".into()]))
        .await
        .unwrap();
    let ids: Vec<&str> = both.iter().map(|a| a.identity.as_str()).collect();
    assert_eq!(both.len(), 1);
    assert_eq!(ids[0], "alpha");

    // No matches
    let none = storage
        .find_agents(&AgentFilter::new().capabilities(vec!["nonexistent".into()]))
        .await
        .unwrap();
    assert!(none.is_empty());
}

#[tokio::test]
async fn test_find_agents_alive_filter() {
    let storage = setup_storage().await;

    // Register agent whose last_seen is "right now"
    storage
        .register_agent(record("live-agent", &["compute"]))
        .await
        .unwrap();

    // Within a 60-second window — should appear
    let fresh = storage
        .find_agents(&AgentFilter::new().alive_within(60))
        .await
        .unwrap();
    let fresh_ids: Vec<&str> = fresh.iter().map(|a| a.identity.as_str()).collect();
    assert!(fresh_ids.contains(&"live-agent"));

    // Within a 1-second window — should also appear immediately
    let now_window = storage
        .find_agents(&AgentFilter::new().alive_within(1))
        .await
        .unwrap();
    assert!(now_window.iter().any(|a| a.identity == "live-agent"));

    // Now backdate the agent by writing a stale last_seen, then verify
    // the 1-second window excludes it.
    let stale = AgentRecord {
        last_seen: Utc::now() - chrono::Duration::seconds(30),
        ..record("live-agent", &["compute"])
    };
    storage.register_agent(stale).await.unwrap();

    let after_stale = storage
        .find_agents(&AgentFilter::new().alive_within(1))
        .await
        .unwrap();
    assert!(
        !after_stale.iter().any(|a| a.identity == "live-agent"),
        "agent with last_seen 30s ago should be excluded from a 1s window"
    );
}

#[tokio::test]
async fn test_get_agent_not_found() {
    let storage = setup_storage().await;
    let result = storage.get_agent("does-not-exist").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_touch_agent_updates_liveness() {
    let storage = setup_storage().await;

    // Register with a stale last_seen
    let stale = AgentRecord {
        last_seen: Utc::now() - chrono::Duration::seconds(120),
        ..record("touched-agent", &["compute"])
    };
    storage.register_agent(stale).await.unwrap();

    // Outside 60s window initially
    let before = storage
        .find_agents(&AgentFilter::new().alive_within(60))
        .await
        .unwrap();
    assert!(
        !before.iter().any(|a| a.identity == "touched-agent"),
        "stale agent should not appear in 60s window before touch"
    );

    // Touch to refresh
    storage.touch_agent("touched-agent").await.unwrap();

    let after = storage
        .find_agents(&AgentFilter::new().alive_within(60))
        .await
        .unwrap();
    assert!(
        after.iter().any(|a| a.identity == "touched-agent"),
        "touched agent should appear in 60s window"
    );
}

#[tokio::test]
async fn test_deregister_agent() {
    let storage = setup_storage().await;
    storage
        .register_agent(record("ephemeral", &["compute"]))
        .await
        .unwrap();

    // Sanity: it exists
    assert!(storage.get_agent("ephemeral").await.unwrap().is_some());

    // Deregister
    storage.deregister_agent("ephemeral").await.unwrap();

    // Gone
    assert!(storage.get_agent("ephemeral").await.unwrap().is_none());
}

// ── In-memory AgentRegistry tests ───────────────────────────

#[tokio::test]
async fn test_inmem_register_and_list() {
    let reg = AgentRegistry::new();
    reg.register("alpha".to_string(), vec!["compute".into()])
        .await;
    reg.register("beta".to_string(), vec!["observe".into()])
        .await;

    let agents = reg.list().await;
    assert_eq!(agents.len(), 2);
    let ids: Vec<&str> = agents.iter().map(|a| a.identity.as_str()).collect();
    assert!(ids.contains(&"alpha"));
    assert!(ids.contains(&"beta"));
}

#[tokio::test]
async fn test_inmem_find_by_capability() {
    let reg = AgentRegistry::new();
    reg.register(
        "alpha".to_string(),
        vec!["compute".into(), "observe".into()],
    )
    .await;
    reg.register("beta".to_string(), vec!["compute".into()])
        .await;
    reg.register("gamma".to_string(), vec!["observe".into()])
        .await;

    let compute_only = reg.find_by_capability(&["compute".to_string()]).await;
    let ids: Vec<&str> = compute_only.iter().map(|a| a.identity.as_str()).collect();
    assert_eq!(compute_only.len(), 2);
    assert!(ids.contains(&"alpha"));
    assert!(ids.contains(&"beta"));

    let both = reg
        .find_by_capability(&["compute".to_string(), "observe".to_string()])
        .await;
    let ids: Vec<&str> = both.iter().map(|a| a.identity.as_str()).collect();
    assert_eq!(both.len(), 1);
    assert_eq!(ids[0], "alpha");

    // Empty caps -> return all
    let all = reg.find_by_capability(&[]).await;
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn test_inmem_find_alive() {
    let reg = AgentRegistry::new();
    reg.register("alpha".to_string(), vec!["compute".into()])
        .await;

    // Within 60s — should appear
    let fresh = reg.find_alive(60).await;
    assert_eq!(fresh.len(), 1);

    // Wait 2 seconds, then within 1s window — should not appear
    tokio::time::sleep(Duration::from_secs(2)).await;
    let stale = reg.find_alive(1).await;
    assert!(
        stale.is_empty(),
        "alpha should fall out of 1s window after 2s"
    );

    // Still appears in 5s window
    let still_fresh = reg.find_alive(5).await;
    assert_eq!(still_fresh.len(), 1);
}

#[tokio::test]
async fn test_inmem_touch_updates_liveness() {
    let reg = AgentRegistry::new();
    reg.register("alpha".to_string(), vec!["compute".into()])
        .await;

    // Wait, then touch
    tokio::time::sleep(Duration::from_secs(2)).await;
    reg.touch("alpha").await;

    // Should appear in 1s window
    let live = reg.find_alive(1).await;
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].identity, "alpha");
}

#[tokio::test]
async fn test_inmem_deregister() {
    let reg = AgentRegistry::new();
    reg.register("alpha".to_string(), vec!["compute".into()])
        .await;

    assert!(reg.deregister("alpha").await);
    let all = reg.list().await;
    assert!(all.is_empty());

    // Second deregister returns false
    assert!(!reg.deregister("alpha").await);
}
