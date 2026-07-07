//! Unit tests for structured progress event helpers.

use chrono::Utc;
use nats_hub::events::{event_summary, event_type, format_event_line, resolve_watch_target, WatchQuery};
use nats_hub::{Envelope, MessageKind, Meta};

fn sample_event(event_type_str: &str, data: serde_json::Value) -> Envelope {
    Envelope {
        meta: Meta {
            id: "evt-1".to_string(),
            from: "hermes-worker-1".to_string(),
            channel: "session.a3f7b2c1".to_string(),
            to: None,
            timestamp: Utc::now(),
            kind: MessageKind::Event,
            reply_to: None,
        },
        payload: serde_json::json!({
            "event_type": event_type_str,
            "data": data,
        }),
    }
}

#[test]
fn test_event_summary_completed() {
    let env = sample_event("completed", serde_json::json!({"result": "7*8=56."}));
    assert_eq!(event_type(&env), Some("completed"));
    assert_eq!(event_summary(&env), "7*8=56.");
}

#[test]
fn test_format_event_line_includes_sender() {
    let env = sample_event("started", serde_json::json!({"prompt": "What is 7*8?"}));
    let line = format_event_line(&env);
    assert!(line.contains("started"));
    assert!(line.contains("hermes-worker-1"));
    assert!(line.contains("What is 7*8?"));
}

#[test]
fn test_resolve_watch_target_session() {
    let target = resolve_watch_target(&WatchQuery {
        session: Some("a3f7b2c1".to_string()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(target.subject, "channel.session.a3f7b2c1");
}

#[test]
fn test_resolve_watch_target_wave() {
    let target = resolve_watch_target(&WatchQuery {
        wave: Some("wave-001".to_string()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(target.subject, "channel.>");
    assert_eq!(
        target.channel_prefix.as_deref(),
        Some("wave.wave-001")
    );
}

#[test]
fn test_resolve_watch_target_requires_scope() {
    assert!(resolve_watch_target(&WatchQuery::default()).is_err());
}
