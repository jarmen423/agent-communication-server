//! Unit tests for the router core (no NATS server needed).

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::mirror::{MirrorOp, StorageMirror};
use super::RouterCore;
use crate::protocol::{Envelope, MessageKind};
use crate::storage::{HistoryQuery, Storage};
use crate::{MetricsCollector, SurrealStorage};

fn bytes(env: &Envelope) -> Vec<u8> {
    env.to_json_bytes().unwrap()
}

fn register_payload(identity: &str, caps: &[&str]) -> Vec<u8> {
    bytes(&Envelope::new(
        identity,
        "hub.register",
        MessageKind::Control,
        json!({"identity": identity, "capabilities": caps}),
    ))
}

fn presence_payload(identity: &str) -> Vec<u8> {
    bytes(&Envelope::new(
        identity,
        "hub.presence",
        MessageKind::Status,
        json!({"identity": identity}),
    ))
}

async fn memory_storage() -> Arc<dyn Storage> {
    let s = SurrealStorage::connect_memory().await.unwrap();
    s.migrate().await.unwrap();
    Arc::new(s)
}

#[tokio::test]
async fn heartbeat_preserves_capabilities() {
    let core = RouterCore::new();
    core.on_register("hub.register", &register_payload("w1", &["code", "review"]))
        .await;
    core.on_presence("hub.presence", &presence_payload("w1"))
        .await;
    core.on_presence("hub.presence", &presence_payload("w1"))
        .await;

    let agents = core.registry.list().await;
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].capabilities, vec!["code", "review"]);
    assert_eq!(
        core.registry
            .find_by_capability(&["code".into()])
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn heartbeat_from_unknown_agent_adds_it_without_caps() {
    let core = RouterCore::new();
    core.on_presence("hub.presence", &presence_payload("ghost"))
        .await;
    let agents = core.registry.list().await;
    assert_eq!(agents.len(), 1);
    assert!(agents[0].capabilities.is_empty());
}

#[tokio::test]
async fn reregistration_dedupes_and_replaces_routing_entries() {
    let core = RouterCore::new();
    core.on_register("hub.register", &register_payload("w1", &["code", "code"]))
        .await;
    core.on_register("hub.register", &register_payload("w1", &["code"]))
        .await;
    assert_eq!(core.routing.subscribers("code").await, vec!["w1"]);

    // New capability set replaces the old one.
    core.on_register("hub.register", &register_payload("w1", &["review"]))
        .await;
    assert!(core.routing.subscribers("code").await.is_empty());
    assert_eq!(core.routing.subscribers("review").await, vec!["w1"]);
}

#[tokio::test]
async fn on_send_routes_dm_and_broadcast() {
    let core = RouterCore::new();
    let dm = Envelope::new("a", "tasks", MessageKind::Message, json!({})).to("b");
    let (dest, _, _) = core.on_send("hub.send.tasks", &bytes(&dm)).unwrap();
    assert_eq!(dest, "channel.inbox.b");

    let bc = Envelope::new("a", "tasks", MessageKind::Message, json!({}));
    let (dest, _, _) = core.on_send("hub.send.tasks", &bytes(&bc)).unwrap();
    assert_eq!(dest, "channel.tasks");

    assert!(core.on_send("hub.send.tasks", b"not json").is_none());
}

#[tokio::test]
async fn mirror_overflow_drops_and_counts() {
    let metrics = Arc::new(MetricsCollector::default());
    let mirror = StorageMirror::new(memory_storage().await, 2, Some(metrics.clone()));

    // Writer not started: the queue holds 2, the rest are dropped.
    let mut accepted = 0;
    for i in 0..5 {
        let env = Envelope::new("a", "c", MessageKind::Message, json!({ "i": i }));
        if mirror.enqueue(MirrorOp::Envelope(Box::new(env))) {
            accepted += 1;
        }
    }
    assert_eq!(accepted, 2);
    assert_eq!(mirror.dropped(), 3);
    assert_eq!(metrics.mirror_dropped(), 3);
    assert!(metrics
        .render_prometheus()
        .contains("natshub_storage_mirror_dropped_total 3"));
}

#[tokio::test]
async fn mirror_writer_persists_in_order() {
    let storage = memory_storage().await;
    let core = RouterCore::new();
    let _ = core
        .mirror
        .set(StorageMirror::new(storage.clone(), 64, None));
    core.mirror.get().unwrap().start().await;

    core.on_register("hub.register", &register_payload("w1", &["code"]))
        .await;
    core.on_presence("hub.presence", &presence_payload("w1"))
        .await;
    for i in 0..10 {
        let env = Envelope::new("w1", "mirror.test", MessageKind::Message, json!({ "i": i }));
        let (_, env, _) = core.on_send("hub.send.mirror.test", &bytes(&env)).unwrap();
        core.mirror_envelope(env);
    }

    // Single writer drains asynchronously; poll until it catches up.
    let q = HistoryQuery::new().channel("mirror.test");
    let mut stored = 0;
    for _ in 0..100 {
        stored = storage.query_history(&q).await.unwrap().len();
        if stored == 10 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(stored, 10);

    // Heartbeat (touch) after register kept the persisted capabilities.
    let agent = storage.get_agent("w1").await.unwrap().unwrap();
    assert_eq!(agent.capabilities, vec!["code"]);
}

// ── Identity binding (iteration-2 T1) ──────────────────────────────

#[tokio::test]
async fn bound_subject_overwrites_meta_from_and_rebytes() {
    let core = RouterCore::new();
    let mut env = Envelope::new("mallory", "chat", MessageKind::Message, json!({"t": 1})).to("bob");
    // forged claim in the body; subject says alice
    env.meta.from = "mallory".into();
    let original = bytes(&env);

    let (dest, routed, publish) = core
        .on_send("hub.pub.alice.chat", &original)
        .expect("bound subject should route");
    assert_eq!(dest, "channel.inbox.bob");
    assert_eq!(routed.meta.from, "alice", "subject identity must win");
    // downstream consumers get the rewritten bytes, not the forged original
    let on_wire = Envelope::from_json_bytes(&publish).unwrap();
    assert_eq!(on_wire.meta.from, "alice");
    assert_ne!(publish, original, "publish bytes must be re-serialized");
}

#[tokio::test]
async fn bound_subject_invalid_identity_drops() {
    let core = RouterCore::new();
    let env = Envelope::new("alice", "chat", MessageKind::Message, json!({}));
    // identity token present but not [A-Za-z0-9_-]+
    assert!(core.on_send("hub.pub.a b.chat", &bytes(&env)).is_none());
    assert!(core.on_send("hub.pub.a*.chat", &bytes(&env)).is_none());
    assert!(core.on_send("hub.pub..chat", &bytes(&env)).is_none());
    // multi-token channel is legal: identity 'a', channel 'b.chat'
    assert!(core.on_send("hub.pub.a.b.chat", &bytes(&env)).is_some());
}

#[tokio::test]
async fn legacy_subject_passthrough_in_permissive_mode() {
    let core = RouterCore::new();
    let env = Envelope::new("self-asserted", "chat", MessageKind::Message, json!({}));
    let payload = bytes(&env);
    let (dest, routed, publish) = core.on_send("hub.send.chat", &payload).unwrap();
    assert_eq!(dest, "channel.chat");
    assert_eq!(routed.meta.from, "self-asserted", "compat: from untouched");
    assert_eq!(publish, payload, "compat: bytes untouched");
}

#[tokio::test]
async fn require_bound_drops_legacy_subjects() {
    let mut core = RouterCore::new();
    core.require_bound = true;

    let env = Envelope::new("alice", "chat", MessageKind::Message, json!({}));
    assert!(
        core.on_send("hub.send.chat", &bytes(&env)).is_none(),
        "legacy hub.send must drop under require-bound"
    );
    assert!(
        core.on_send("hub.pub.alice.chat", &bytes(&env)).is_some(),
        "bound subject still routes under require-bound"
    );

    // register/presence on legacy subjects are dropped; bound ones land
    core.on_register("hub.register", &register_payload("w-legacy", &["code"]))
        .await;
    assert!(!has(&core, "w-legacy").await);
    core.on_register(
        "hub.register.w-bound",
        &register_payload("w-legacy", &["code"]),
    )
    .await;
    // subject identity wins over payload.identity
    assert!(has(&core, "w-bound").await);
    assert!(!has(&core, "w-legacy").await);

    core.on_presence("hub.presence", &presence_payload("p-legacy"))
        .await;
    assert!(!has(&core, "p-legacy").await);
    core.on_presence("hub.presence.p-bound", &presence_payload("p-legacy"))
        .await;
    assert!(has(&core, "p-bound").await);
}

#[tokio::test]
async fn bound_register_overrides_payload_identity() {
    let core = RouterCore::new();
    // payload claims 'mallory' but the subject binds 'alice'
    core.on_register("hub.register.alice", &register_payload("mallory", &["x"]))
        .await;
    assert!(has(&core, "alice").await);
    assert!(!has(&core, "mallory").await);
}

async fn has(core: &RouterCore, identity: &str) -> bool {
    core.registry
        .list()
        .await
        .iter()
        .any(|a| a.identity == identity)
}
