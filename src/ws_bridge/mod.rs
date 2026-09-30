//! WebSocket bridge — streams live NATS events to browser visualizers.
//!
//! hub-server spawns a WS listener on `--ws-addr` (e.g. 127.0.0.1:9191).
//! Each WS client gets a live feed of all `channel.>` envelopes as JSON.
//! Browser clients may also send control commands (message/stop/resume)
//! which are published onto the NATS bus via HubClient.

mod commands;
mod http;
mod ws;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

/// Shared broadcast channel for envelope events (router → browsers).
pub type EventTx = tokio::sync::broadcast::Sender<String>;

/// Create a bounded broadcast channel for envelope events.
pub fn create_event_channel(capacity: usize) -> EventTx {
    tokio::sync::broadcast::channel(capacity).0
}

/// Start the WebSocket bridge server on the given address.
///
/// - `event_tx`: broadcast sender that the router pushes JSON envelopes to
/// - `static_dir`: optional directory to serve static files from (visualizer HTML)
/// - `nats_url`: when set, browser → hub commands (message/stop/resume) publish to NATS
pub async fn start_ws_bridge(
    addr: &str,
    event_tx: EventTx,
    static_dir: Option<PathBuf>,
    nats_url: Option<String>,
) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!("WS bridge listening on http://{addr}");

    let static_dir = Arc::new(static_dir);
    let nats_url = Arc::new(nats_url);

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
        let nats = nats_url.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, tx, sd, nats, peer_addr).await {
                debug!("WS connection from {peer_addr} ended: {e}");
            }
        });
    }
}

async fn handle_connection(
    stream: TcpStream,
    event_tx: EventTx,
    static_dir: Arc<Option<PathBuf>>,
    nats_url: Arc<Option<String>>,
    peer: std::net::SocketAddr,
) -> Result<()> {
    let mut peek_buf = [0u8; 4096];
    let n = stream.peek(&mut peek_buf).await?;
    let request = String::from_utf8_lossy(&peek_buf[..n]);

    let is_ws = request.contains("Upgrade: websocket")
        || request.contains("upgrade: websocket")
        || request
            .lines()
            .next()
            .map(|l| l.contains("/ws"))
            .unwrap_or(false);

    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    if is_ws || path == "/ws" {
        ws::handle_websocket(stream, event_tx, nats_url, peer).await
    } else {
        http::handle_http(stream, path, &static_dir).await
    }
}
