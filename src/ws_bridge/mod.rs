//! WebSocket bridge — streams live NATS events to browser visualizers.
//!
//! hub-server spawns a WS listener on `--ws-addr` (e.g. 127.0.0.1:9191).
//! Each WS client gets a live feed of all `channel.>` envelopes as JSON.
//! Browser clients may also send control commands (message/stop/resume)
//! which are published onto the NATS bus via HubClient.
//!
//! The bridge is a privileged endpoint: it reads all bus traffic and can
//! publish commands that spawn workers. Hardening lives in three places —
//! an Origin allowlist and an optional `?token=` secret enforced during the
//! WS upgrade (see `ws.rs`), and canonicalization + traversal checks on the
//! static file server (see `http.rs`).

mod commands;
mod http;
mod ws;

use anyhow::Result;
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};

use crate::HubClient;

pub use http::resolve_static_path;

/// Shared broadcast channel for envelope events (router → browsers).
pub type EventTx = tokio::sync::broadcast::Sender<String>;

/// Create a bounded broadcast channel for envelope events.
pub fn create_event_channel(capacity: usize) -> EventTx {
    tokio::sync::broadcast::channel(capacity).0
}

/// Configuration for the WS bridge.
pub struct WsBridgeConfig {
    /// Directory to serve static files from (visualizer HTML/JS/CSS).
    pub static_dir: Option<PathBuf>,
    /// NATS URL for browser → hub commands (message/stop/resume).
    /// None = read-only event feed.
    pub nats_url: Option<String>,
    /// Exact `Origin` header values allowed on the WS upgrade
    /// (e.g. `http://127.0.0.1:9191`). Requests carrying a different Origin
    /// are rejected with 403; requests with no Origin header are not checked
    /// (browsers always send one — use `token` to gate non-browser clients).
    pub allowed_origins: Vec<String>,
    /// Shared secret required as `?token=` on the WS upgrade URL.
    /// None = no token check (acceptable on loopback only).
    pub token: Option<String>,
    /// Sender identity stamped on envelopes the bridge publishes
    /// (was hard-coded "josh").
    pub identity: String,
}

/// Per-bridge state shared by every accepted connection.
struct BridgeState {
    event_tx: EventTx,
    /// Canonicalized static root; None disables static serving.
    static_root: Option<PathBuf>,
    /// One shared NATS client for all browser command channels.
    hub: Option<Arc<HubClient>>,
    allowed_origins: Vec<String>,
    token: Option<String>,
    identity: String,
}

/// Start the WebSocket bridge server on the given address.
///
/// - `event_tx`: broadcast sender that the router pushes JSON envelopes to
/// - `config`: static dir, NATS URL, Origin allowlist, token, identity
pub async fn start_ws_bridge(addr: &str, event_tx: EventTx, config: WsBridgeConfig) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;
    info!("WS bridge listening on http://{addr}");
    serve_ws_bridge(listener, event_tx, config).await
}

/// Serve the WS bridge on an already-bound listener.
///
/// Callers bind `TcpListener::bind("127.0.0.1:0")` in tests to learn the port
/// before serving; `start_ws_bridge` is the usual entry point.
pub async fn serve_ws_bridge(
    listener: TcpListener,
    event_tx: EventTx,
    config: WsBridgeConfig,
) -> Result<()> {
    let static_root = match &config.static_dir {
        Some(dir) => match dir.canonicalize() {
            Ok(canon) => Some(canon),
            Err(e) => {
                warn!(
                    "--static-dir {} cannot be resolved ({e}); static serving disabled",
                    dir.display()
                );
                None
            }
        },
        None => None,
    };

    // One NATS connection shared by all browser tabs. Commands are disabled
    // bridge-wide if it can't be established at startup.
    let hub = match config.nats_url.as_ref() {
        Some(url) => match HubClient::connect(url, "visualizer").await {
            Ok(c) => Some(Arc::new(c)),
            Err(e) => {
                warn!("WS bridge could not connect HubClient for commands: {e}");
                None
            }
        },
        None => None,
    };

    let state = Arc::new(BridgeState {
        event_tx,
        static_root,
        hub,
        allowed_origins: config.allowed_origins,
        token: config.token,
        identity: config.identity,
    });

    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                warn!("WS accept error: {e}");
                continue;
            }
        };

        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, st, peer_addr).await {
                debug!("WS connection from {peer_addr} ended: {e}");
            }
        });
    }
}

/// True when `addr` resolves only to loopback IPs (`127.0.0.1:9191`,
/// `localhost:9191`, `[::1]:9191`). Unresolvable, empty, or partially
/// non-loopback answers are false — the safe choice for the `--ws-addr`
/// no-token guard.
pub fn is_loopback_addr(addr: &str) -> bool {
    let addrs: Vec<_> = match addr.to_socket_addrs() {
        Ok(it) => it.collect(),
        Err(_) => return false,
    };
    !addrs.is_empty() && addrs.iter().all(|a| a.ip().is_loopback())
}

async fn handle_connection(
    stream: TcpStream,
    state: Arc<BridgeState>,
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
        ws::handle_websocket(stream, state, peer).await
    } else {
        http::handle_http(stream, path, &state).await
    }
}
