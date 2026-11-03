//! Router stamping of `meta.origin` and the oversize guard (no NATS needed).

use std::sync::Arc;

use serde_json::json;

use super::RouterCore;
use crate::manifest::{IdentityKind, IdentityManifest, ManifestRecord, OwnerKind, Role};
use crate::protocol::{Envelope, MessageKind};
use crate::MetricsCollector;

fn bytes(env: &Envelope) -> Vec<u8> {
    env.to_json_bytes().unwrap()
}

/// A router with a tenant and one manifest record, for `prov-a`.
async fn stamping_core() -> RouterCore {
    let manifest = IdentityManifest::default();
    let record = ManifestRecord {
        identity: "prov-a".into(),
        owner_kind: OwnerKind::User,
        user: Some("alice".into()),
        user_id: None,
        device: None,
        device_id: None,
        kind: IdentityKind::Device,
        agent: Some("claude-code".into()),
        role: Role::Worker,
        accept: None,
        rev: 1,
    };
    manifest.put(None, record).await.unwrap();
    let mut core = RouterCore::new();
    core.manifest = Arc::new(manifest);
    core.tenant = Some("acme".into());
    core
}

/// Wire JSON for an envelope that claims `origin`.
fn with_origin(mut env: Envelope, origin: serde_json::Value) -> Vec<u8> {
    env.meta.origin = serde_json::from_value(origin).unwrap();
    bytes(&env)
}

#[tokio::test]
async fn on_send_output_bytes_carry_the_stamped_origin() {
    let core = stamping_core().await;
    let env = Envelope::new("prov-a", "chat", MessageKind::Message, json!({}));
    let payload = with_origin(
        env,
        json!({"user": "forged", "tenant": "forged", "reported": {"model": "m1"}}),
    );

    let (_, routed, publish) = core.on_send("hub.pub.prov-a.chat", &payload).unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&publish).unwrap();
    assert_eq!(
        wire["meta"]["origin"],
        json!({
            "tenant": "acme",
            "identity": "prov-a",
            "user": "alice",
            "owner_kind": "user",
            "agent": "claude-code",
            "reported": {"model": "m1"},
        })
    );
    assert_eq!(routed.meta.origin.unwrap().user.as_deref(), Some("alice"));
}

#[tokio::test]
async fn legacy_send_gets_only_tenant_and_reported() {
    let core = stamping_core().await;
    // `meta.from` names a manifest identity, but a legacy subject proves nothing.
    let env = Envelope::new("prov-a", "chat", MessageKind::Message, json!({}));
    let payload = with_origin(
        env,
        json!({"identity": "forged", "reported": {"model": "m1"}}),
    );

    let (_, _, publish) = core.on_send("hub.send.chat", &payload).unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&publish).unwrap();
    assert_eq!(
        wire["meta"]["origin"],
        json!({"tenant": "acme", "reported": {"model": "m1"}})
    );
}

#[tokio::test]
async fn ws_feed_carries_the_stamped_origin() {
    let mut core = stamping_core().await;
    let tx = crate::ws_bridge::create_event_channel(4);
    let mut rx = tx.subscribe();
    core.ws_event_tx = Some(tx);

    let env = Envelope::new("prov-a", "chat", MessageKind::Message, json!({}));
    core.on_send("hub.pub.prov-a.chat", &bytes(&env)).unwrap();

    let feed: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
    assert_eq!(feed["meta"]["origin"]["user"], "alice");
    assert_eq!(feed["meta"]["origin"]["tenant"], "acme");
}

/// An envelope claiming `reported.model`, and its encoded size once stamped
/// by `core` with `reported` removed.
fn oversize_case(core: &RouterCore) -> (Envelope, usize) {
    let mut env = Envelope::new(
        "prov-a",
        "chat",
        MessageKind::Message,
        json!({"pad": "x".repeat(1_000)}),
    );
    env.meta.origin = serde_json::from_value(json!({"reported": {"model": "m1"}})).unwrap();
    let (_, mut routed, with_reported) = core
        .on_send("hub.pub.prov-a.chat", &bytes(&env))
        .expect("fits the default limit");
    routed.meta.origin.as_mut().unwrap().reported = Default::default();
    let without_reported = bytes(&routed).len();
    assert!(without_reported < with_reported.len());
    (env, without_reported)
}

#[tokio::test]
async fn oversize_guard_strips_reported_first() {
    let mut core = stamping_core().await;
    let (env, without_reported) = oversize_case(&core);
    let metrics = Arc::new(MetricsCollector::default());
    core.metrics = Some(metrics.clone());
    core.max_payload = without_reported;

    let (_, routed, publish) = core
        .on_send("hub.pub.prov-a.chat", &bytes(&env))
        .expect("should route without reported");
    assert_eq!(publish.len(), without_reported);
    let origin = Envelope::from_json_bytes(&publish)
        .unwrap()
        .meta
        .origin
        .unwrap();
    assert!(origin.reported.is_empty(), "reported is dropped first");
    assert_eq!(origin.user.as_deref(), Some("alice"), "trusted fields stay");
    assert!(routed.meta.origin.unwrap().reported.is_empty());
    assert_eq!(metrics.oversize_dropped(), 0);
    assert_eq!(metrics.messages_total(), 1);
}

#[tokio::test]
async fn oversize_guard_drops_and_counts_when_still_too_large() {
    let mut core = stamping_core().await;
    let (env, without_reported) = oversize_case(&core);
    let metrics = Arc::new(MetricsCollector::default());
    core.metrics = Some(metrics.clone());
    let tx = crate::ws_bridge::create_event_channel(4);
    let mut rx = tx.subscribe();
    core.ws_event_tx = Some(tx);
    core.max_payload = without_reported - 1;

    assert!(core.on_send("hub.pub.prov-a.chat", &bytes(&env)).is_none());
    assert_eq!(metrics.oversize_dropped(), 1);
    assert_eq!(metrics.messages_total(), 0, "not counted as routed");
    assert!(
        rx.try_recv().is_err(),
        "a dropped envelope stays off the feed"
    );
    assert!(metrics
        .render_prometheus()
        .contains("nats_hub_router_oversize_dropped_total 1"));
}

#[tokio::test]
async fn durable_routing_leaves_room_for_its_message_id_header() {
    let mut core = stamping_core().await;
    let (_, _, publish) = core
        .on_send(
            "hub.pub.prov-a.chat",
            &bytes(&Envelope::new(
                "prov-a",
                "chat",
                MessageKind::Message,
                json!({}),
            )),
        )
        .unwrap();

    // A plain publish may use the whole limit.
    core.max_payload = publish.len();
    let env = Envelope::new("prov-a", "chat", MessageKind::Message, json!({}));
    assert!(core.on_send("hub.pub.prov-a.chat", &bytes(&env)).is_some());

    // A durable one also has to fit its header, or the republish would fail
    // and be retried for good.
    core.reserve_router_header = true;
    let env = Envelope::new("prov-a", "chat", MessageKind::Message, json!({}));
    assert!(core.on_send("hub.pub.prov-a.chat", &bytes(&env)).is_none());

    // The reserve covers the header the durable router really sends.
    let mut headers = async_nats::HeaderMap::new();
    let msg_id = crate::delivery::router_msg_id(&env.meta.from, &env.meta.id);
    headers.insert(crate::delivery::MSG_ID_HEADER, msg_id.as_str());
    let reserve = super::state::ROUTER_HEADER_OVERHEAD + env.meta.from.len() + env.meta.id.len();
    let wire_len = "NATS/1.0\r\n".len() + "Nats-Msg-Id: ".len() + msg_id.len() + "\r\n\r\n".len();
    assert!(wire_len <= reserve, "{wire_len} > {reserve}");
}
