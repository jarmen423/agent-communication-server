//! WebSocket endpoint — upgrades a connection and pumps the shared
//! envelope broadcast to the browser, and browser commands back onto NATS.

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::net::TcpStream;
use tracing::{debug, warn};

use super::commands::handle_client_command;
use super::EventTx;
use crate::HubClient;

pub(super) async fn handle_websocket(
    stream: TcpStream,
    event_tx: EventTx,
    nats_url: Arc<Option<String>>,
    _peer: std::net::SocketAddr,
) -> Result<()> {
    let ws_stream = tokio_tungstenite::accept_async(stream).await?;
    let (mut ws_tx, mut ws_rx) = ws_stream.split();
    let mut rx = event_tx.subscribe();

    // Optional outbound client so browser buttons can publish onto the bus.
    let hub = match nats_url.as_ref() {
        Some(url) => match HubClient::connect(url, "visualizer").await {
            Ok(c) => Some(c),
            Err(e) => {
                warn!("WS bridge could not connect HubClient for commands: {e}");
                None
            }
        },
        None => None,
    };

    debug!("WS client connected (commands={})", hub.is_some());

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
                        if let Some(ref client) = hub {
                            match handle_client_command(client, &text).await {
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
