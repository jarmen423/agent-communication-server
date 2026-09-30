//! Integration tests for the Analytics trait (Phase 4a).
//!
//! Uses an in-memory SurrealDB (`connect_memory`) so no server or file is
//! needed. Envelopes are seeded with explicit timestamps/kind/to/reply_to
//! so the aggregations are deterministic.

use chrono::{Duration, Utc};
use nats_hub::analytics::{Analytics, Interval, SurrealAnalytics, TimeRange};
use nats_hub::protocol::{Envelope, MessageKind, Meta};
use nats_hub::{Storage, SurrealStorage};
use serde_json::json;
use std::sync::Arc;

async fn setup() -> SurrealAnalytics {
    let storage = SurrealStorage::connect_memory().await.unwrap();
    storage.migrate().await.unwrap();
    SurrealAnalytics::new(Arc::new(storage) as Arc<dyn Storage>)
}

/// Build an envelope with a controlled timestamp (so buckets are deterministic).
fn env_at(
    from: &str,
    channel: &str,
    kind: MessageKind,
    ts: chrono::DateTime<Utc>,
    payload: serde_json::Value,
) -> Envelope {
    Envelope {
        meta: Meta {
            id: uuid::Uuid::new_v4().to_string(),
            from: from.to_string(),
            channel: channel.to_string(),
            to: None,
            timestamp: ts,
            kind,
            reply_to: None,
        },
        payload,
    }
}

#[tokio::test]
async fn test_message_rate_buckets() {
    let a = setup().await;
    let base = Utc::now() - Duration::minutes(5);

    // 4 in minute 0, 6 in minute 1 (within two distinct minutes).
    // Note: query_history uses `timestamp > since` (strict), so seed strictly
    // after `base`.
    for i in 0..4 {
        let e = env_at(
            "x",
            "agents.broadcast",
            MessageKind::Message,
            base + Duration::seconds(1 + i * 5),
            json!({}),
        );
        a.storage().store_envelope(&e).await.unwrap();
    }
    for i in 0..6 {
        let e = env_at(
            "x",
            "agents.broadcast",
            MessageKind::Message,
            base + Duration::seconds(61 + i * 5),
            json!({}),
        );
        a.storage().store_envelope(&e).await.unwrap();
    }

    let range = TimeRange::bounded(base, base + Duration::minutes(3));
    let points = a.message_rate(&range, Interval::Minute).await.unwrap();
    let total: u64 = points.iter().map(|p| p.count).sum();
    assert_eq!(total, 10, "all 10 seeded messages should be counted");
}

#[tokio::test]
async fn test_channel_hotspots() {
    let a = setup().await;
    let now = Utc::now();

    for _ in 0..5 {
        let e = env_at("x", "hot.channel", MessageKind::Message, now, json!({}));
        a.storage().store_envelope(&e).await.unwrap();
    }
    for _ in 0..2 {
        let e = env_at("x", "cold.channel", MessageKind::Message, now, json!({}));
        a.storage().store_envelope(&e).await.unwrap();
    }
    for _ in 0..1 {
        let e = env_at("x", "mid.channel", MessageKind::Message, now, json!({}));
        a.storage().store_envelope(&e).await.unwrap();
    }

    let range = TimeRange::last(60);
    let hotspots = a.channel_hotspots(&range, 1).await.unwrap();
    assert_eq!(hotspots.len(), 1);
    assert_eq!(hotspots[0].channel, "hot.channel");
    assert_eq!(hotspots[0].messages, 5);
}

#[tokio::test]
async fn test_agent_activity() {
    let a = setup().await;
    let now = Utc::now();

    // 3 sent
    for _ in 0..3 {
        let e = env_at(
            "agent-x",
            "agents.broadcast",
            MessageKind::Message,
            now,
            json!({}),
        );
        a.storage().store_envelope(&e).await.unwrap();
    }
    // 2 received DM (to = agent-x)
    for _ in 0..2 {
        let mut e = env_at(
            "other",
            "inbox.agent-x",
            MessageKind::Message,
            now,
            json!({}),
        );
        e.meta.to = Some("agent-x".to_string());
        a.storage().store_envelope(&e).await.unwrap();
    }
    // 1 event from agent-x
    let ev = env_at(
        "agent-x",
        "session.s1",
        MessageKind::Event,
        now,
        json!({"event_type": "started", "data": {}}),
    );
    a.storage().store_envelope(&ev).await.unwrap();
    // 1 pending DM (to = agent-x, no reply)
    let mut pend = env_at(
        "boss",
        "inbox.agent-x",
        MessageKind::Message,
        now,
        json!({"task": "do"}),
    );
    pend.meta.to = Some("agent-x".to_string());
    a.storage().store_envelope(&pend).await.unwrap();

    let range = TimeRange::last(60);
    let act = a.agent_activity("agent-x", &range).await.unwrap();
    assert_eq!(act.sent, 4, "3 messages + 1 event");
    // received_dm counts every DM addressed to agent-x (2 received + 1 pending)
    assert_eq!(act.received_dm, 3, "2 received DMs + 1 pending DM");
    assert_eq!(act.events, 1);
    // agent-x has 3 unanswered DMs in its inbox (2 received + 1 pending) — all
    // are "pending" because none have a reply addressed back to the senders.
    assert_eq!(act.pending, 3, "all 3 DMs to agent-x are unanswered");
}

#[tokio::test]
async fn test_latency_stats() {
    let a = setup().await;
    let t0 = Utc::now() - Duration::seconds(1000);

    let original = env_at(
        "worker",
        "agents.tasks",
        MessageKind::Message,
        t0,
        json!({"prompt": "hi"}),
    );
    let original_id = original.meta.id.clone();
    a.storage().store_envelope(&original).await.unwrap();

    // reply 500s later
    let mut reply = env_at(
        "worker",
        "agents.tasks",
        MessageKind::Message,
        t0 + Duration::seconds(500),
        json!({"result": "ok"}),
    );
    reply.meta.to = Some("boss".to_string());
    reply.meta.reply_to = Some(original_id);
    a.storage().store_envelope(&reply).await.unwrap();

    let range = TimeRange::bounded(t0 - Duration::seconds(10), t0 + Duration::seconds(600));
    let stats = a.latency_stats(None, &range).await.unwrap();
    assert_eq!(stats.samples, 1);
    assert!((stats.avg_ms - 500_000.0).abs() < 1.0, "avg ~500s");
    assert!((stats.p50_ms - 500_000.0).abs() < 1.0);
    assert!((stats.max_ms - 500_000.0).abs() < 1.0);
}

#[tokio::test]
async fn test_error_rate() {
    let a = setup().await;
    let now = Utc::now();

    for _ in 0..3 {
        let e = env_at(
            "worker",
            "session.s1",
            MessageKind::Event,
            now,
            json!({"event_type": "error", "data": {"error": "boom"}}),
        );
        a.storage().store_envelope(&e).await.unwrap();
    }
    for _ in 0..2 {
        let e = env_at(
            "worker",
            "session.s1",
            MessageKind::Event,
            now,
            json!({"event_type": "completed", "data": {"result": "ok"}}),
        );
        a.storage().store_envelope(&e).await.unwrap();
    }

    let range = TimeRange::last(60);
    let points = a.error_rate(&range, Interval::Minute).await.unwrap();
    let total: u64 = points.iter().map(|p| p.count).sum();
    assert_eq!(total, 3, "only the 3 error events counted");
}

#[tokio::test]
async fn test_time_range_filter() {
    let a = setup().await;
    let base = Utc::now() - Duration::minutes(10);

    // inside window
    let inside = env_at(
        "x",
        "agents.broadcast",
        MessageKind::Message,
        base + Duration::seconds(30),
        json!({}),
    );
    a.storage().store_envelope(&inside).await.unwrap();

    // before window
    let before = env_at(
        "x",
        "agents.broadcast",
        MessageKind::Message,
        base - Duration::seconds(120),
        json!({}),
    );
    a.storage().store_envelope(&before).await.unwrap();

    let range = TimeRange::bounded(base, base + Duration::minutes(5));
    let points = a.message_rate(&range, Interval::Minute).await.unwrap();
    let total: u64 = points.iter().map(|p| p.count).sum();
    assert_eq!(total, 1, "only the in-window message counts");
}
