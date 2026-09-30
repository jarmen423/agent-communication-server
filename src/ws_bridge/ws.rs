//! WebSocket endpoint — upgrades a connection and pumps the shared
//! envelope broadcast to the browser, and browser commands back onto NATS.
//!
//! The upgrade is gated twice, inside the handshake callback so a rejection
//! is a real HTTP response, not a dropped socket:
//! - `Origin` must match `state.allowed_origins` when the header is present
//!   (browsers always send it). → 403
//! - `?token=` must equal `state.token` when one is configured. → 401

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tracing::{debug, warn};

use super::commands::handle_client_command;
use super::config::normalize_origin;
use super::http::percent_decode;
use super::BridgeState;

pub(super) async fn handle_websocket(
    stream: TcpStream,
    state: Arc<BridgeState>,
    _peer: std::net::SocketAddr,
) -> Result<()> {
    let gate = state.clone();
    let ws_stream =
        tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, resp: Response| {
            check_upgrade(req, &gate).map(|_| resp)
        })
        .await?;
    let (mut ws_tx, mut ws_rx) = ws_stream.split();
    let mut rx = state.event_tx.subscribe();

    debug!("WS client connected");

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(json) => {
                        if ws_tx
                            .send(tokio_tungstenite::tungstenite::Message::Text(json))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        let warn_msg = format!(r#"{{"type":"lagged","skipped":{n}}}"#);
                        let _ = ws_tx
                            .send(tokio_tungstenite::tungstenite::Message::Text(warn_msg))
                            .await;
                    }
                    Err(_) => break,
                }
            }
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => break,
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                        // Read per command: the shared client is filled in
                        // once the background connect-retry succeeds.
                        let hub = state.hub.read().await.clone();
                        if let Some(ref client) = hub {
                            match handle_client_command(client, &text, &state.identity).await {
                                Ok(ack) => {
                                    let _ = ws_tx
                                        .send(tokio_tungstenite::tungstenite::Message::Text(ack))
                                        .await;
                                }
                                Err(e) => {
                                    let err = format!(
                                        r#"{{"type":"error","message":{}}}"#,
                                        serde_json::to_string(&e.to_string()).unwrap_or_else(|_| "\"error\"".into())
                                    );
                                    let _ = ws_tx
                                        .send(tokio_tungstenite::tungstenite::Message::Text(err))
                                        .await;
                                }
                            }
                        } else {
                            let err = r#"{"type":"error","message":"NATS commands disabled on this bridge"}"#;
                            let _ = ws_tx
                                .send(tokio_tungstenite::tungstenite::Message::Text(err.into()))
                                .await;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }

    debug!("WS client disconnected");
    Ok(())
}

/// Origin allowlist + token check, run inside the WS handshake callback.
/// Returning `Err(ErrorResponse)` aborts the upgrade with that response.
// ErrorResponse is tungstenite's handshake-rejection type; its size is not ours to shrink.
#[allow(clippy::result_large_err)]
fn check_upgrade(req: &Request, state: &BridgeState) -> Result<(), ErrorResponse> {
    if let Some(value) = req.headers().get("Origin") {
        match value.to_str() {
            Ok(origin) => {
                let origin = normalize_origin(origin);
                if !state.allowed_origins.iter().any(|o| *o == origin) {
                    warn!("WS upgrade rejected: Origin '{origin}' not in allowlist");
                    return Err(reject(
                        StatusCode::FORBIDDEN,
                        "forbidden Origin — add it with --ws-allow-origin",
                    ));
                }
            }
            // An Origin we can't even parse must not silently skip the check.
            Err(_) => {
                warn!("WS upgrade rejected: Origin header is not valid UTF-8");
                return Err(reject(StatusCode::FORBIDDEN, "malformed Origin header"));
            }
        }
    }

    if let Some(expected) = state.token.as_deref() {
        let ok = req
            .uri()
            .query()
            .and_then(|q| query_param(q, "token"))
            .is_some_and(|t| token_eq(&t, expected));
        if !ok {
            warn!("WS upgrade rejected: missing or invalid token");
            return Err(reject(
                StatusCode::UNAUTHORIZED,
                "missing or invalid token — use /ws?token=…",
            ));
        }
    }

    Ok(())
}

/// Constant-time-ish string equality for the shared token: never early-exits
/// on a matching prefix, so response timing doesn't reveal how much of the
/// guess was right. (Length is still leaked — standard for this pattern.)
fn token_eq(given: &str, expected: &str) -> bool {
    let (a, b) = (given.as_bytes(), expected.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// First `name=value` query pair, percent-decoded.
fn query_param(query: &str, name: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        if it.next() == Some(name) {
            return Some(percent_decode(it.next().unwrap_or("")));
        }
    }
    None
}

fn reject(status: StatusCode, body: &str) -> ErrorResponse {
    Response::builder()
        .status(status)
        .header("Content-Type", "text/plain")
        .body(Some(body.to_string()))
        .expect("static rejection response must build")
}
