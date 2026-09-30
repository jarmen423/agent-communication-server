//! Unit tests for the router's pure routing decision and routing table.
//! No NATS needed.

use nats_hub::router::{channel_from_send_subject, route_subject};
use nats_hub::{Envelope, MessageKind, RoutingTable};
use serde_json::json;

fn env(channel: &str) -> Envelope {
    Envelope::new("alice", channel, MessageKind::Message, json!({"x": 1}))
}

#[test]
fn dm_routes_to_recipient_inbox() {
    let e = env("tasks").to("bob");
    assert_eq!(route_subject("hub.send.tasks", &e), "channel.inbox.bob");
}

#[test]
fn broadcast_routes_to_channel() {
    let e = env("agents.broadcast");
    assert_eq!(
        route_subject("hub.send.agents.broadcast", &e),
        "channel.agents.broadcast"
    );
}

#[test]
fn dm_ignores_channel_even_on_task_channels() {
    let e = env("task.123").to("worker-1");
    assert_eq!(
        route_subject("hub.send.task.123", &e),
        "channel.inbox.worker-1"
    );
}

#[test]
fn broadcast_on_task_and_session_channels() {
    let e = env("task.abc");
    assert_eq!(route_subject("hub.send.task.abc", &e), "channel.task.abc");
    let e = env("session.s1");
    assert_eq!(
        route_subject("hub.send.session.s1", &e),
        "channel.session.s1"
    );
    let e = env("wave.w1.task.t1");
    assert_eq!(
        route_subject("hub.send.wave.w1.task.t1", &e),
        "channel.wave.w1.task.t1"
    );
}

#[test]
fn broadcast_channel_comes_from_subject_not_meta() {
    // meta.channel disagrees with the subject: the subject wins (unchanged
    // router semantics).
    let e = env("claimed");
    assert_eq!(route_subject("hub.send.actual", &e), "channel.actual");
}

#[test]
fn reply_to_does_not_affect_routing() {
    let e = env("task.9").reply_to("some-message-id");
    assert_eq!(route_subject("hub.send.task.9", &e), "channel.task.9");
    let e = env("task.9").reply_to("some-message-id").to("orch");
    assert_eq!(route_subject("hub.send.task.9", &e), "channel.inbox.orch");
}

#[test]
fn subject_without_send_prefix_is_unknown() {
    assert_eq!(channel_from_send_subject("hub.send.x.y"), "x.y");
    assert_eq!(channel_from_send_subject("hub.send"), "unknown");
    assert_eq!(channel_from_send_subject("other.subject"), "unknown");
    assert_eq!(channel_from_send_subject("hub.sendx.y"), "unknown");
    assert_eq!(route_subject("weird", &env("c")), "channel.unknown");
}

#[tokio::test]
async fn routing_table_dedupes_subscribers() {
    let t = RoutingTable::new();
    t.add_subscriber("code", "w1").await;
    t.add_subscriber("code", "w1").await;
    t.add_subscriber("code", "w2").await;
    assert_eq!(t.subscribers("code").await, vec!["w1", "w2"]);
}

#[tokio::test]
async fn routing_table_set_capabilities_replaces() {
    let t = RoutingTable::new();
    t.set_capabilities("w1", &["code".into(), "review".into()])
        .await;
    t.set_capabilities("w2", &["code".into()]).await;
    t.set_capabilities("w1", &["review".into(), "review".into()])
        .await;
    assert_eq!(t.subscribers("code").await, vec!["w2"]);
    assert_eq!(t.subscribers("review").await, vec!["w1"]);

    t.remove_identity("w2").await;
    assert!(t.subscribers("code").await.is_empty());
}
