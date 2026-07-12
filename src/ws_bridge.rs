//! WebSocket bridge — streams live NATS events to browser visualizers.
//!
//! hub-server spawns a WS listener on `--ws-addr` (e.g. 127.0.0.1:9191).
//! Each WS client gets a live feed of all `channel.>` envelopes as JSON.
//! Browser clients may also send control commands (message/stop/resume)
//! which are published onto the NATS bus via HubClient.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::protocol::{Envelope, MessageKind};
use crate::HubClient;

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
        handle_websocket(stream, event_tx, nats_url, peer).await
    } else {
        handle_http(stream, path, &static_dir).await
    }
}

async fn handle_websocket(
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

/// Browser command → NATS publish.
/// Supported:
/// - `{"type":"send_message","to":"agent","message":"...","provider":"grok"?}` → ensure worker then task
/// - `{"type":"stop_agent","identity":"..."}` → stop supervised worker + status closed
/// - `{"type":"resume_agent","identity":"..."}` → status ready on agents.<id>
/// - `{"type":"ensure_worker","identity":"...","provider":"..."}` → spawn only
async fn handle_client_command(client: &HubClient, text: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(text).context("invalid JSON command")?;
    let cmd = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match cmd {
        "send_message" => {
            let to = v
                .get("to")
                .and_then(|t| t.as_str())
                .context("send_message requires to")?;
            let message = v
                .get("message")
                .and_then(|t| t.as_str())
                .context("send_message requires message")?;
            let provider = v
                .get("provider")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string());
            let ensure = v
                .get("ensure_worker")
                .and_then(|t| t.as_bool())
                .unwrap_or(true);

            let mut ensure_status = serde_json::Value::Null;
            if ensure {
                if let Some(ref prov) = provider {
                    match client
                        .request_json(
                            "hub.worker.ensure",
                            serde_json::json!({
                                "identity": to,
                                "provider": prov,
                            }),
                            std::time::Duration::from_secs(45),
                        )
                        .await
                    {
                        Ok(resp) => {
                            ensure_status = resp.clone();
                            if resp.get("ok") == Some(&serde_json::Value::Bool(false)) {
                                return Ok(format!(
                                    r#"{{"type":"error","message":"worker ensure failed","detail":{}}}"#,
                                    resp
                                ));
                            }
                        }
                        Err(e) => {
                            // Supervisor down — still deliver the task (manual workers may exist)
                            warn!("worker ensure failed (continuing to publish task): {e}");
                            ensure_status = serde_json::json!({
                                "ok": false,
                                "error": e.to_string(),
                                "continued": true,
                            });
                        }
                    }
                }
            }

            let task_short = &Uuid::new_v4().to_string()[..8];
            let task_channel = format!("task.{task_short}");
            let payload = serde_json::json!({
                "prompt": message,
                "source": "visualizer",
                "provider": provider,
            });
            let env = Envelope::new("josh", &task_channel, MessageKind::Message, payload).to(to);
            client.send(&env).await?;

            Ok(format!(
                r#"{{"type":"ack","action":"send_message","to":{},"task_channel":{},"ensure":{}}}"#,
                serde_json::to_string(to).unwrap(),
                serde_json::to_string(&task_channel).unwrap(),
                ensure_status
            ))
        }
        "ensure_worker" => {
            let identity = v
                .get("identity")
                .and_then(|t| t.as_str())
                .context("ensure_worker requires identity")?;
            let provider = v
                .get("provider")
                .and_then(|t| t.as_str())
                .context("ensure_worker requires provider")?;
            match client
                .request_json(
                    "hub.worker.ensure",
                    serde_json::json!({ "identity": identity, "provider": provider }),
                    std::time::Duration::from_secs(45),
                )
                .await
            {
                Ok(resp) => Ok(format!(
                    r#"{{"type":"ack","action":"ensure_worker","detail":{}}}"#,
                    resp
                )),
                Err(e) => Ok(format!(
                    r#"{{"type":"error","message":"ensure_worker failed: {}"}}"#,
                    e.to_string().replace('"', "'")
                )),
            }
        }
        "stop_agent" => {
            let identity = v
                .get("identity")
                .and_then(|t| t.as_str())
                .context("stop_agent requires identity")?;
            let _ = client
                .request_json(
                    "hub.worker.stop",
                    serde_json::json!({ "identity": identity }),
                    std::time::Duration::from_secs(10),
                )
                .await;
            let channel = format!("agents.{identity}");
            client.send_status(&channel, "closed").await?;
            let _ = client
                .send_message(
                    &channel,
                    serde_json::json!({
                        "message": format!("visualizer stop requested for {identity}"),
                        "action": "stop",
                        "source": "visualizer",
                    }),
                )
                .await;
            Ok(format!(
                r#"{{"type":"ack","action":"stop_agent","identity":{}}}"#,
                serde_json::to_string(identity).unwrap()
            ))
        }
        "resume_agent" => {
            let identity = v
                .get("identity")
                .and_then(|t| t.as_str())
                .context("resume_agent requires identity")?;
            let channel = format!("agents.{identity}");
            client.send_status(&channel, "ready").await?;
            let _ = client
                .send_message(
                    &channel,
                    serde_json::json!({
                        "message": format!("visualizer resume requested for {identity}"),
                        "action": "resume",
                        "source": "visualizer",
                    }),
                )
                .await;
            Ok(format!(
                r#"{{"type":"ack","action":"resume_agent","identity":{}}}"#,
                serde_json::to_string(identity).unwrap()
            ))
        }
        other => Ok(format!(
            r#"{{"type":"error","message":"unknown command: {}"}}"#,
            other.replace('"', "'")
        )),
    }
}

async fn handle_http(
    mut stream: TcpStream,
    path: &str,
    static_dir: &Arc<Option<PathBuf>>,
) -> Result<()> {
    let mut buf = [0u8; 4096];
    let _ = stream.read(&mut buf).await;

    let dir = match static_dir.as_ref() {
        Some(d) => d,
        None => {
            let body = "Static file serving disabled. Use --static-dir.";
            write_http_response(&mut stream, 404, "Not Found", "text/plain", body.as_bytes())
                .await?;
            return Ok(());
        }
    };

    let file_path = if path == "/" {
        dir.join("index.html")
    } else {
        dir.join(path.trim_start_matches('/'))
    };

    if !file_path.starts_with(dir) {
        write_http_response(
            &mut stream,
            403,
            "Forbidden",
            "text/plain",
            b"Path traversal denied",
        )
        .await?;
        return Ok(());
    }

    if file_path.exists() {
        let content = tokio::fs::read(&file_path).await?;
        let mime = mime_type(&file_path);
        write_http_response(&mut stream, 200, "OK", mime, &content).await?;
    } else {
        write_http_response(&mut stream, 404, "Not Found", "text/plain", b"404 Not Found")
            .await?;
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
    // no-cache so visualizer HTML edits show up without fighting browser cache
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store, max-age=0\r\n\r\n",
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
