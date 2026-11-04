//! Unit tests for structured progress event helpers.

use chrono::Utc;
use nats_hub::events::{
    event_summary, event_type, format_event_line, origin_label, resolve_watch_target, WatchQuery,
};
use nats_hub::{Envelope, MessageKind, Meta, Origin, Reported};

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
            origin: None,
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
    assert_eq!(target.channel_prefix.as_deref(), Some("wave.wave-001"));
}

#[test]
fn test_resolve_watch_target_requires_scope() {
    assert!(resolve_watch_target(&WatchQuery::default()).is_err());
}

#[test]
fn test_origin_label_full() {
    let origin = Origin {
        user: Some("alice".into()),
        agent: Some("claude-code".into()),
        reported: Reported {
            model: Some("claude-opus-5-5".into()),
            session: Some("9f2c1a2b3c4d".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        origin_label(&origin, false),
        "alice · claude-code · ~claude-opus-5-5"
    );
    assert_eq!(
        origin_label(&origin, true),
        "alice · claude-code · ~claude-opus-5-5 · ~session:9f2c1a2b"
    );
}

#[test]
fn test_origin_label_trusted_only() {
    let origin = Origin {
        user: Some("alice".into()),
        agent: Some("claude-code".into()),
        ..Default::default()
    };
    assert_eq!(origin_label(&origin, false), "alice · claude-code");
    assert_eq!(origin_label(&origin, true), "alice · claude-code");
}

#[test]
fn test_origin_label_reported_only_is_marked_unverified() {
    let origin = Origin {
        reported: Reported {
            model: Some("some-model".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(origin_label(&origin, false), "~some-model");
}

#[test]
fn test_origin_label_none_is_empty() {
    assert_eq!(origin_label(&Origin::default(), false), "");
    assert_eq!(origin_label(&Origin::default(), true), "");
}

#[test]
fn test_origin_label_strips_control_characters() {
    let origin = Origin {
        user: Some("al\x1bice\r\n".into()),
        reported: Reported {
            model: Some("mo\x07del".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let label = origin_label(&origin, false);
    assert_eq!(label, "alice · ~model");
    assert!(!label.chars().any(char::is_control));
}

#[test]
fn test_format_event_line_appends_origin_label() {
    let mut env = sample_event("started", serde_json::json!({"prompt": "hi"}));
    env.meta.origin = Some(Origin {
        user: Some("alice".into()),
        reported: Reported {
            model: Some("some-model".into()),
            ..Default::default()
        },
        ..Default::default()
    });
    assert!(format_event_line(&env).contains("alice · ~some-model"));
}
