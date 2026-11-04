//! Regression checks for admission boundaries and durable routing.
use super::*;
use crate::protocol::{Envelope, MessageKind};
use serde_json::json;
use std::sync::{atomic::Ordering, Arc};

#[test]
fn invalid_identifiers_and_reserved_legacy_senders_are_dropped() {
    let mut core = RouterCore::new();
    let metrics = Arc::new(MetricsCollector::default());
    core.metrics = Some(metrics.clone());
    for bad in ["", "a\r\nb", "space here", "é", &"a".repeat(129)] {
        for reply in [false, true] {
            let mut env = Envelope::new("alice", "task.safe", MessageKind::Message, json!({}));
            if reply {
                env.meta.reply_to = Some(bad.into());
            } else {
                env.meta.id = bad.into();
            }
            assert!(core
                .prepare_send("hub.pub.alice.task.safe", &env.to_json_bytes().unwrap())
                .is_none());
        }
    }
    assert_eq!(metrics.invalid_message_id_total.load(Ordering::Relaxed), 10);
    for id in crate::protocol::RESERVED_IDENTITIES {
        let env = Envelope::new(*id, "chat", MessageKind::Message, json!({}));
        assert!(core
            .prepare_send("hub.send.chat", &env.to_json_bytes().unwrap())
            .is_none());
    }
}

#[tokio::test]
async fn oversize_refusals_are_handled_and_durable_ws_only_follows_new_ack() {
    if std::env::var("NATS_HUB_DURABLE").is_ok() {
        return;
    }
    let Ok(url) = std::env::var("NATS_URL") else {
        return;
    };
    let manifest = Arc::new(IdentityManifest::new(["writer".into()].into()));
    let mut router = ControlPlane::connect(&url)
        .await
        .unwrap()
        .with_manifest(manifest);
    for action in ["cancel", "session_close"] {
        let cleanup = Envelope::new(
            "hub-orchestrator",
            "task.safe",
            MessageKind::Control,
            json!({"action":action}),
        )
        .to("missing");
        assert!(matches!(
            router
                .accept_send(
                    "hub.pub.hub-orchestrator.task.safe",
                    &cleanup.to_json_bytes().unwrap(),
                    None
                )
                .await
                .unwrap(),
            policy::SendOutcome::Routed(..)
        ));
        assert!(matches!(
            router
                .accept_send(
                    "hub.pub.bob.task.safe",
                    &cleanup.to_json_bytes().unwrap(),
                    None
                )
                .await
                .unwrap(),
            policy::SendOutcome::Rejected
        ));
    }
    router.core.max_payload = 100;
    let metrics = Arc::new(MetricsCollector::default());
    router.core.metrics = Some(metrics.clone());
    let env = Envelope::new("alice", "task.safe", MessageKind::Message, json!({})).to("missing");
    assert!(matches!(
        router
            .accept_send(
                "hub.pub.alice.task.safe",
                &env.to_json_bytes().unwrap(),
                None
            )
            .await
            .unwrap(),
        policy::SendOutcome::Rejected
    ));
    assert_eq!(metrics.oversize_dropped(), 2);

    let ingress = router
        .start_durable(Arc::new(crate::delivery::DeliveryConfig::default()))
        .await
        .unwrap();
    assert!(matches!(
        router
            .accept_send(
                "hub.pub.alice.task.safe",
                &env.to_json_bytes().unwrap(),
                Some(&ingress)
            )
            .await
            .unwrap(),
        policy::SendOutcome::Rejected
    ));
    assert_eq!(metrics.oversize_dropped(), 4);
    // Observe the ingress acknowledgement itself: an oversize notice must
    // Ack the refused delivery, whereas malformed IDs must Term it.
    let ack_subject = router.nats.new_inbox();
    let mut acknowledgements = router.nats.subscribe(ack_subject.clone()).await.unwrap();
    router.nats.flush().await.unwrap();
    let message = async_nats::Message {
        subject: "hub.pub.alice.task.safe".into(),
        reply: Some(ack_subject.into()),
        payload: env.to_json_bytes().unwrap().into(),
        headers: None,
        status: None,
        description: None,
        length: 0,
    };
    let delivery = async_nats::jetstream::Message {
        message,
        context: ingress.js.clone(),
    };
    router.handle_durable(&ingress, delivery.clone()).await;
    use futures_util::StreamExt;
    let ack = tokio::time::timeout(std::time::Duration::from_secs(2), acknowledgements.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ack.payload.as_ref(), b"+ACK");
    assert_eq!(metrics.oversize_dropped(), 6);
    let mut bad = env.clone();
    bad.meta.id = "bad\r\nid".into();
    let mut delivery = delivery;
    delivery.message.payload = bad.to_json_bytes().unwrap().into();
    router.handle_durable(&ingress, delivery).await;
    let ack = tokio::time::timeout(std::time::Duration::from_secs(2), acknowledgements.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ack.payload.as_ref(), b"+TERM");

    router.core.max_payload = 1 << 20;
    router.core.manifest = Arc::new(IdentityManifest::default());
    router.core.reserve_router_header = true;
    let tx = crate::ws_bridge::create_event_channel(16);
    let mut ws = tx.subscribe();
    router.core.ws_event_tx = Some(tx);
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let env = Envelope::new(
        "alice",
        format!("task.{suffix}"),
        MessageKind::Message,
        json!({}),
    );
    let subject = format!("hub.pub.alice.task.{suffix}");
    // Pull a real ingress message, then process that same delivery again to
    // simulate a crash after destination ack but before ingress ack.
    router
        .nats
        .publish(subject, env.to_json_bytes().unwrap().into())
        .await
        .unwrap();
    router.nats.flush().await.unwrap();
    let mut messages = router.ingress_messages(&ingress).await.unwrap();
    loop {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(3), messages.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if Envelope::from_json_bytes(&msg.payload).is_ok_and(|e| e.meta.id == env.meta.id) {
            assert!(ws.try_recv().is_err());
            router.handle_durable(&ingress, msg.clone()).await;
            assert_eq!(
                serde_json::from_str::<Envelope>(&ws.recv().await.unwrap())
                    .unwrap()
                    .meta
                    .id,
                env.meta.id
            );
            router.handle_durable(&ingress, msg).await;
            assert!(ws.try_recv().is_err());
            break;
        }
    }
    router.stop_delivery().await.unwrap();
}
