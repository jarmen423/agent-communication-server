//! WebSocket bridge — streams live NATS events to browser visualizers.
//!
//! hub-server spawns a WS listener on `--ws-addr` (e.g. 127.0.0.1:9191).
//! Each WS client gets a live feed of all `channel.>` envelopes as JSON.
//! The browser visualizer (p5.js arcade) consumes these to animate agents.
//!
//! Also serves static files (visualizer HTML/JS) when `--static-dir` is set.

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

/// A shared broadcast channel for envelope events.
/// hub-server's router pushes envelopes here; WS clients consume.
pub type EventTx = tokio::sync::broadcast::Sender<String>;

/// Create a bounded broadcast channel for envelope events.
pub fn create_event_channel(capacity: usize) -> EventTx {
    tokio::sync::broadcast::channel(capacity).0
}

/// Start the WebSocket bridge server on the given address.
///
/// - `event_tx`: broadcast sender that the router pushes JSON envelopes to
/// - `static_dir`: optional directory to serve static files from (visualizer HTML)
pub async fn start_ws_bridge(
    addr: &str,
    event_tx: EventTx,
    static_dir: Option<PathBuf>,
) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!("WS bridge listening on http://{addr}");

    let static_dir = Arc::new(static_dir);
    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                warn!("WS accept error: {e}");
                continue;
            }
        };

        let tx = event_tx.clone();
        let sd = static_dir.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, tx, sd, peer_addr).await {
                debug!("WS connection from {peer_addr} ended: {e}");
            }
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    event_tx: EventTx,
    static_dir: Arc<Option<PathBuf>>,
    peer: std::net::SocketAddr,
) -> Result<()> {
    // Peek at the request to determine if it's a WebSocket upgrade or HTTP
    let mut peek_buf = [0u8; 4096];
    let n = stream.peek(&mut peek_buf).await?;
    let request = String::from_utf8_lossy(&peek_buf[..n]);

    let is_ws = request.contains("Upgrade: websocket")
        || request.contains("upgrade: websocket")
        || request.lines().next().map(|l| l.contains("/ws")).unwrap_or(false);

    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    if is_ws || path == "/ws" {
        handle_websocket(stream, event_tx, peer).await
    } else {
        handle_http(stream, path, &static_dir).await
    }
}

async fn handle_websocket(
    stream: TcpStream,
    event_tx: EventTx,
    _peer: std::net::SocketAddr,
) -> Result<()> {
    let ws_stream = tokio_tungstenite::accept_async(stream).await?;
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    let mut rx = event_tx.subscribe();

    debug!("WS client connected");

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(json) => {
                        if ws_tx.send(tokio_tungstenite::tungstenite::Message::Text(json)).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        let warn_msg = format!("{{\"type\":\"lagged\",\"skipped\":{n}}}");
                        let _ = ws_tx.send(tokio_tungstenite::tungstenite::Message::Text(warn_msg)).await;
                    }
                    Err(_) => break,
                }
            }
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    debug!("WS client disconnected");
    Ok(())
}

async fn handle_http(
    mut stream: TcpStream,
    path: &str,
    static_dir: &Arc<Option<PathBuf>>,
) -> Result<()> {
    // Read and discard the full request
    let mut buf = [0u8; 4096];
    let _ = stream.read(&mut buf).await;

    let dir = match static_dir.as_ref() {
        Some(d) => d,
        None => {
            let body = "Static file serving disabled. Use --static-dir.";
            write_http_response(&mut stream, 404, "Not Found", "text/plain", body.as_bytes()).await?;
            return Ok(());
        }
    };

    let file_path = if path == "/" {
        dir.join("index.html")
    } else {
        dir.join(path.trim_start_matches('/'))
    };

    if !file_path.starts_with(dir) {
        write_http_response(&mut stream, 403, "Forbidden", "text/plain", b"Path traversal denied").await?;
        return Ok(());
    }

    if file_path.exists() {
        let content = tokio::fs::read(&file_path).await?;
        let mime = mime_type(&file_path);
        write_http_response(&mut stream, 200, "OK", mime, &content).await?;
    } else {
        write_http_response(&mut stream, 404, "Not Found", "text/plain", b"404 Not Found").await?;
    }

    Ok(())
}

async fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    Ok(())
}

fn mime_type(path: &PathBuf) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript",
        Some("css") => "text/css",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}
