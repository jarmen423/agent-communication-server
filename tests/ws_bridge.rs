//! WS bridge hardening tests — static-path traversal, Origin allowlist,
//! and token auth on the upgrade. Runs an in-process bridge on port 0;
//! no NATS needed (nats_url is left unset).

use nats_hub::ws_bridge::{
    create_event_channel, is_loopback_addr, resolve_static_path, serve_ws_bridge, WsBridgeConfig,
};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::ClientRequestBuilder;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::tungstenite::Error;

fn test_config() -> WsBridgeConfig {
    WsBridgeConfig {
        static_dir: None,
        nats_url: None,
        allowed_origins: vec![],
        token: None,
        identity: "human".into(),
    }
}

/// Serve a bridge on 127.0.0.1:0 and return the bound addr + event sender.
async fn spawn_bridge(config: WsBridgeConfig) -> (SocketAddr, nats_hub::ws_bridge::EventTx) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let tx = create_event_channel(16);
    let tx2 = tx.clone();
    tokio::spawn(async move {
        let _ = serve_ws_bridge(listener, tx2, config).await;
    });
    (addr, tx)
}

/// Raw HTTP GET; returns (status code, body).
async fn http_get(addr: SocketAddr, request_target: &str) -> (u16, Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(format!("GET {request_target} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let head = String::from_utf8_lossy(&buf);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body = head
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or("")
        .as_bytes()
        .to_vec();
    (status, body)
}

fn ws_uri(addr: SocketAddr, path: &str) -> Uri {
    format!("ws://{addr}{path}").parse().unwrap()
}

/// Status code of a failed WS upgrade, if the server rejected it cleanly.
fn reject_status(err: &Error) -> Option<u16> {
    match err {
        Error::Http(resp) => Some(resp.status().as_u16()),
        _ => None,
    }
}

// ── Static file resolution ──────────────────────────────────────

#[test]
fn resolver_denies_traversal_and_allows_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("static");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("index.html"), "x").unwrap();
    std::fs::write(tmp.path().join("secret.txt"), "s").unwrap();
    let root = root.canonicalize().unwrap();

    assert!(resolve_static_path(&root, "/index.html").is_some());
    assert_eq!(resolve_static_path(&root, "/../secret.txt"), None);
    assert_eq!(resolve_static_path(&root, "/../../etc/passwd"), None);
    assert_eq!(resolve_static_path(&root, "/%2e%2e/secret.txt"), None);
}

#[tokio::test]
async fn static_server_serves_index_and_denies_traversal() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("static");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("index.html"), "<h1>viz</h1>").unwrap();
    std::fs::write(tmp.path().join("secret.txt"), "top secret").unwrap();

    let mut cfg = test_config();
    cfg.static_dir = Some(root);
    let (addr, _tx) = spawn_bridge(cfg).await;

    let (status, body) = http_get(addr, "/").await;
    assert_eq!(status, 200);
    assert_eq!(body, b"<h1>viz</h1>");

    // Page URL with the visualizer token parameter still resolves to index.
    let (status, _body) = http_get(addr, "/?token=abc").await;
    assert_eq!(status, 200);

    for path in [
        "/../secret.txt",
        "/../../etc/passwd",
        "/%2e%2e/secret.txt",
        "/%2e%2e%2f%2e%2e%2fetc%2fpasswd",
    ] {
        let (status, _body) = http_get(addr, path).await;
        assert_eq!(status, 403, "path: {path}");
    }

    let (status, _body) = http_get(addr, "/missing.js").await;
    assert_eq!(status, 404);
}

// ── Origin allowlist ────────────────────────────────────────────

#[tokio::test]
async fn ws_upgrade_enforces_origin_allowlist() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let tx = create_event_channel(16);

    let mut cfg = test_config();
    cfg.allowed_origins = vec![format!("http://{addr}")];
    tokio::spawn(async move {
        let _ = serve_ws_bridge(listener, tx, cfg).await;
    });

    // No Origin header (non-browser client): allowed through.
    tokio_tungstenite::connect_async(ws_uri(addr, "/ws"))
        .await
        .expect("no-origin upgrade should succeed");

    // Allowed Origin.
    let good = ClientRequestBuilder::new(ws_uri(addr, "/ws"))
        .with_header("Origin", format!("http://{addr}"));
    tokio_tungstenite::connect_async(good)
        .await
        .expect("allowed origin should succeed");

    // Anything else → 403 before the upgrade.
    let evil =
        ClientRequestBuilder::new(ws_uri(addr, "/ws")).with_header("Origin", "http://evil.example");
    match tokio_tungstenite::connect_async(evil).await {
        Err(e) => assert_eq!(reject_status(&e), Some(403), "err: {e:?}"),
        Ok(_) => panic!("cross-origin upgrade should be rejected"),
    }
}

// ── Token on upgrade ────────────────────────────────────────────

#[tokio::test]
async fn ws_upgrade_enforces_token() {
    let mut cfg = test_config();
    cfg.token = Some("sekrit".into());
    let (addr, tx) = spawn_bridge(cfg).await;

    // Missing token → 401.
    match tokio_tungstenite::connect_async(ws_uri(addr, "/ws")).await {
        Err(e) => assert_eq!(reject_status(&e), Some(401), "err: {e:?}"),
        Ok(_) => panic!("tokenless upgrade should be rejected"),
    }

    // Wrong token → 401.
    match tokio_tungstenite::connect_async(ws_uri(addr, "/ws?token=wrong")).await {
        Err(e) => assert_eq!(reject_status(&e), Some(401), "err: {e:?}"),
        Ok(_) => panic!("bad-token upgrade should be rejected"),
    }

    // Correct token → upgrade, then the event pump delivers broadcasts.
    let (mut ws, _resp) = tokio_tungstenite::connect_async(ws_uri(addr, "/ws?token=sekrit"))
        .await
        .expect("valid token should succeed");
    tx.send(r#"{"hello":"bus"}"#.into()).unwrap();
    use futures_util::StreamExt;
    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(msg.into_text().unwrap(), r#"{"hello":"bus"}"#);
}

// ── Loopback guard helper ───────────────────────────────────────

#[test]
fn loopback_addr_detection() {
    assert!(is_loopback_addr("127.0.0.1:9191"));
    assert!(is_loopback_addr("127.0.0.2:9191"));
    assert!(is_loopback_addr("localhost:9191"));
    assert!(is_loopback_addr("[::1]:9191"));
    assert!(!is_loopback_addr("0.0.0.0:9191"));
    assert!(!is_loopback_addr("[::]:9191"));
    assert!(!is_loopback_addr("8.8.8.8:9191"));
    assert!(!is_loopback_addr("no.such.host.invalid:9191"));
}
