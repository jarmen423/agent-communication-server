//! Unit tests for the live `MetricsCollector` (sub-phase 4b).
//!
//! These exercise the in-memory collector directly (no NATS, no DB) — they
//! verify counter increments and the Prometheus exposition render.

use chrono::Utc;
use nats_hub::analytics::metrics::{ChannelClass, MetricsCollector};
use nats_hub::protocol::{Envelope, MessageKind, Meta};
use serde_json::json;

/// Build an envelope with the given channel/kind/payload for metric recording.
fn env(channel: &str, kind: MessageKind, payload: serde_json::Value) -> Envelope {
    Envelope {
        meta: Meta {
            id: uuid::Uuid::new_v4().to_string(),
            from: "tester".to_string(),
            channel: channel.to_string(),
            to: None,
            timestamp: Utc::now(),
            kind,
            reply_to: None,
        },
        payload,
    }
}

#[test]
fn test_record_increments() {
    let m = MetricsCollector::default();

    // A plain broadcast message
    m.record(&env(
        "agents.broadcast",
        MessageKind::Message,
        json!({"hi": 1}),
    ));
    // An error event
    m.record(&env(
        "session.s1",
        MessageKind::Event,
        json!({"event_type": "error", "data": {"msg": "boom"}}),
    ));
    // A non-error event (should NOT bump errors_total)
    m.record(&env(
        "session.s1",
        MessageKind::Event,
        json!({"event_type": "progress", "data": {}}),
    ));

    assert_eq!(m.messages_total(), 3);
    // kind counts: 1 message + 2 events
    // by_kind is private; we assert via render output instead.
    let out = m.render_prometheus();
    assert!(out.contains("natshub_messages_by_kind{kind=\"message\"} 1"));
    assert!(out.contains("natshub_messages_by_kind{kind=\"event\"} 2"));
    assert!(out.contains("natshub_errors_total 1"));
}

#[test]
fn test_channel_class() {
    assert_eq!(
        ChannelClass::classify("inbox.worker-1"),
        ChannelClass::Inbox
    );
    assert_eq!(ChannelClass::classify("inbox.x"), ChannelClass::Inbox);
    assert_eq!(ChannelClass::classify("session.abc"), ChannelClass::Session);
    assert_eq!(
        ChannelClass::classify("wave.w1.task.t1"),
        ChannelClass::Wave
    );
    assert_eq!(ChannelClass::classify("task.1234"), ChannelClass::Task);
    assert_eq!(
        ChannelClass::classify("agents.broadcast"),
        ChannelClass::Broadcast
    );
    assert_eq!(ChannelClass::classify("unknown"), ChannelClass::Broadcast);
    assert_eq!(ChannelClass::classify("weirdchannel"), ChannelClass::Other);
}

#[test]
fn test_render_prometheus_bounded_labels() {
    let m = MetricsCollector::default();
    m.record(&env("agents.broadcast", MessageKind::Message, json!({})));
    m.record(&env("inbox.worker-1", MessageKind::Message, json!({})));
    m.record(&env("task.abc", MessageKind::Control, json!({})));

    let out = m.render_prometheus();

    // HELP/TYPE lines present
    assert!(out.contains("# HELP natshub_messages_total"));
    assert!(out.contains("# TYPE natshub_messages_total counter"));
    assert!(out.contains("natshub_messages_total 3"));

    // Bounded channel-class labels only (6 possible values)
    for c in ["broadcast", "inbox", "session", "wave", "task", "other"] {
        assert!(
            out.contains(&format!(
                "natshub_messages_by_channel_class{{class=\"{c}\"}}"
            )),
            "missing channel class label {c}"
        );
    }

    // No per-agent or unbounded label leaked
    assert!(!out.contains("agent="), "per-agent label must not appear");
}

#[test]
fn test_metrics_no_storage_needed() {
    // The collector works without any Storage/DB — purely in-memory atomics.
    let m = MetricsCollector::default();
    for i in 0..50 {
        let ch = if i % 2 == 0 { "agents.x" } else { "task.y" };
        m.record(&env(ch, MessageKind::Message, json!({})));
    }
    assert_eq!(m.messages_total(), 50);

    let out = m.render_prometheus();
    assert!(out.contains("natshub_messages_total 50"));
    // 25 broadcast + 25 task
    assert!(out.contains("natshub_messages_by_channel_class{class=\"broadcast\"} 25"));
    assert!(out.contains("natshub_messages_by_channel_class{class=\"task\"} 25"));
}
