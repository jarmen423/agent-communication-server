//! WS bridge identity (H1 #5): the bridge connects to NATS as its configured
//! `--ws-identity`, so a browser `send_message` reaches the recipient with
//! `meta.from == ws-identity` after the router's bound-subject rewrite (it
//! used to connect as "visualizer", which then won). Needs the live stack
//! (`scripts/dev/with_stack.sh`).

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nats_hub::ws_bridge::{create_event_channel, serve_ws_bridge, WsBridgeConfig};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

fn stack_url() -> Option<String> {
    if std::env::var("NATS_HUB_TEST_STACK").ok().as_deref() != Some("1") {
        eprintln!("Skipping — needs scripts/dev/with_stack.sh (NATS_HUB_TEST_STACK=1)");
        return None;
    }
    Some(std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into()))
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test]
async fn bridge_send_arrives_as_the_ws_identity() {
    let Some(url) = stack_url() else { return };
    let ws_identity = unique("ws-human");
    let rcpt = unique("rcpt");

    let watcher = async_nats::connect(&url).await.unwrap();
    let mut inbox = watcher
        .subscribe(format!("channel.inbox.{rcpt}"))
        .await
        .unwrap();
    watcher.flush().await.unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = WsBridgeConfig {
        static_dir: None,
        nats_url: Some(url.clone()),
        allowed_origins: vec![],
        token: None,
        identity: ws_identity.clone(),
    };
    tokio::spawn(async move {
        let _ = serve_ws_bridge(listener, create_event_channel(16), config).await;
    });

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .expect("ws connect");
    let cmd = json!({"type": "send_message", "to": rcpt, "message": "hi from the browser",
                     "ensure_worker": false})
    .to_string();

    // The bridge's NATS client connects in the background: retry until ack.
    let mut acked = false;
    for _ in 0..50 {
        ws.send(Message::Text(cmd.clone())).await.unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("no reply from bridge")
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
        if v["type"] == "ack" {
            acked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(acked, "bridge never acked send_message");

    let msg = tokio::time::timeout(Duration::from_secs(5), inbox.next())
        .await
        .expect("bridge DM never arrived")
        .unwrap();
    let env: Value = serde_json::from_slice(&msg.payload).unwrap();
    assert_eq!(env["meta"]["from"], ws_identity, "{env}");
    assert_eq!(env["meta"]["to"], rcpt.as_str());
    assert_eq!(env["payload"]["prompt"], "hi from the browser");
}

#[tokio::test]
async fn bridge_refuses_an_invalid_identity() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = WsBridgeConfig {
        static_dir: None,
        nats_url: None,
        allowed_origins: vec![],
        token: None,
        identity: "not a valid.identity".into(),
    };
    let err = serve_ws_bridge(listener, create_event_channel(4), config)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("invalid identity"), "{err}");
}
