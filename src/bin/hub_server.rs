//! hub-server — runs the control plane router.
//!
//! Usage: hub-server [--nats-url URL] [--db-path PATH]
//!
//! Start the NATS server first:  nats-server -c config/nats-server.conf
//! Then:                        cargo run --bin hub-server
//!
//! If `--db-path` is provided, the server attaches a SurrealDB-backed
//! `Storage` for persistent agent registry and message history. The DB
//! file is created on first run; subsequent runs warm the in-memory
//! agent cache from the persisted copy.

use std::io::{Read, Write};
use std::net::{TcpListener, ToSocketAddrs};
use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};
use clap::Parser;
use tracing_subscriber::EnvFilter;

use nats_hub::ControlPlane;
use nats_hub::MetricsCollector;
#[cfg(feature = "storage-surreal")]
use nats_hub::{Storage, SurrealStorage};

#[derive(Parser)]
#[command(name = "hub-server", about = "Run the nats-hub control plane router")]
struct Args {
    /// NATS server URL
    #[arg(long, default_value = "nats://127.0.0.1:4222")]
    nats_url: String,

    /// Path to the SurrealDB persistent store. When provided, the server
    /// attaches a Storage backend for the agent registry and message
    /// history. Omit (or pass an empty value) to run in-memory only.
    #[cfg(feature = "storage-surreal")]
    #[arg(long, default_value = "nats_hub.db")]
    db_path: String,

    /// Address for the Prometheus-compatible `/metrics` endpoint.
    #[arg(long)]
    metrics_addr: Option<String>,

    /// Address for the WebSocket bridge (visualizer). When set (e.g.
    /// `127.0.0.1:9191`), serves a live event stream at /ws and static
    /// files from --static-dir.
    #[arg(long)]
    ws_addr: Option<String>,

    /// Directory to serve static files from (visualizer HTML/JS/CSS).
    #[arg(long)]
    static_dir: Option<String>,

    /// Shared secret required on the WS upgrade as `?token=` in the URL.
    /// Falls back to env HUB_WS_TOKEN. Required for non-loopback --ws-addr
    /// unless --ws-insecure is given.
    #[arg(long)]
    ws_token: Option<String>,

    /// Additional allowed `Origin` for the WS endpoint (repeatable).
    /// `http://<ws-addr>` and `http://localhost:<port>` are always allowed.
    #[arg(long = "ws-allow-origin")]
    ws_allow_origin: Vec<String>,

    /// Sender identity stamped on envelopes the bridge publishes to the bus.
    /// Falls back to env HUB_WS_IDENTITY, then `human`.
    #[arg(long)]
    ws_identity: Option<String>,

    /// Allow a non-loopback --ws-addr without --ws-token. INSECURE: only
    /// for trusted networks.
    #[arg(long)]
    ws_insecure: bool,
}

/// Spawn a minimal HTTP/1.0 server that responds to `GET /metrics` with the
/// Prometheus text exposition format. Uses only `std::net` — no framework.
/// Runs on a dedicated OS thread; clones the `Arc<MetricsCollector>` so it
/// reads live counters.
fn spawn_metrics_server(addr: String, metrics: Arc<MetricsCollector>) {
    thread::spawn(move || {
        let listener = match TcpListener::bind(&addr) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[hub-server] metrics: failed to bind {addr}: {e}");
                return;
            }
        };
        eprintln!("[hub-server] metrics endpoint listening on http://{addr}/metrics");
        // Accept loop — each request is handled inline (metrics scrapes are
        // rare and cheap; no need for a pool).
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let metrics = metrics.clone();
            // Handle each connection in its own thread to avoid blocking the
            // accept loop on a slow client.
            let _ = std::thread::Builder::new()
                .name("metrics-http".into())
                .spawn(move || handle_metrics_conn(&mut stream, &metrics));
        }
    });
}

fn handle_metrics_conn(stream: &mut std::net::TcpStream, metrics: &MetricsCollector) {
    let mut buf = [0u8; 1024];
    // Read just enough to see the request line.
    let _ = stream.read(&mut buf);
    let request = String::from_utf8_lossy(&buf);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("");

    let (status, body) = if path == "/metrics" {
        ("200 OK", metrics.render_prometheus())
    } else {
        ("404 Not Found", String::new())
    };

    let response = format!(
        "HTTP/1.0 {status}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("nats_hub=info".parse()?))
        .init();

    let args = Args::parse();

    // Set up the live metrics collector (always available, storage-independent).
    let metrics = Arc::new(MetricsCollector::default());
    if let Some(ref addr) = args.metrics_addr {
        spawn_metrics_server(addr.clone(), metrics.clone());
    }

    // Set up the WS bridge for the visualizer (browser can also publish commands).
    let ws_tx = nats_hub::ws_bridge::create_event_channel(1024);
    if let Some(ref ws_addr) = args.ws_addr {
        let token = args
            .ws_token
            .clone()
            .or_else(|| std::env::var("HUB_WS_TOKEN").ok())
            .filter(|t| !t.is_empty());
        let identity = args
            .ws_identity
            .clone()
            .or_else(|| std::env::var("HUB_WS_IDENTITY").ok())
            .unwrap_or_else(|| "human".to_string());

        // A tokenless bridge may only bind loopback: it reads all bus
        // traffic and can spawn workers, so an exposed unauthenticated
        // listener is a critical hole.
        if token.is_none() && !args.ws_insecure && !nats_hub::ws_bridge::is_loopback_addr(ws_addr) {
            anyhow::bail!(
                "refusing to start: --ws-addr {ws_addr} is not a loopback address and no \
                 --ws-token (or HUB_WS_TOKEN) is set. Set a token, or pass --ws-insecure to \
                 run unauthenticated on a trusted network."
            );
        }

        let mut allowed_origins = vec![format!("http://{ws_addr}")];
        if let Some(port) = ws_addr
            .to_socket_addrs()
            .ok()
            .and_then(|mut it| it.next())
            .map(|a| a.port())
        {
            allowed_origins.push(format!("http://localhost:{port}"));
        }
        allowed_origins.extend(args.ws_allow_origin.iter().cloned());

        let config = nats_hub::ws_bridge::WsBridgeConfig {
            static_dir: args.static_dir.as_ref().map(std::path::PathBuf::from),
            nats_url: Some(args.nats_url.clone()),
            allowed_origins,
            token: token.clone(),
            identity,
        };
        let ws_tx_clone = ws_tx.clone();
        let ws_addr_clone = ws_addr.clone();
        tokio::spawn(async move {
            if let Err(e) =
                nats_hub::ws_bridge::start_ws_bridge(&ws_addr_clone, ws_tx_clone, config).await
            {
                eprintln!("[hub-server] WS bridge error: {e}");
            }
        });
        match &token {
            Some(t) => info_log(&format!(
                "WS bridge (visualizer) on http://{ws_addr}/?token={t}"
            )),
            None => {
                tracing::warn!(
                    "WS bridge on {ws_addr} has no --ws-token; unauthenticated (loopback-only is safe)"
                );
                info_log(&format!(
                    "WS bridge (visualizer) on http://{ws_addr} (no token; loopback only)"
                ));
            }
        }
    }

    let cp = ControlPlane::connect(&args.nats_url)
        .await
        .context("failed to connect to NATS")?;
    let cp = cp.with_metrics(metrics).with_ws_events(ws_tx);

    #[cfg(feature = "storage-surreal")]
    {
        if !args.db_path.is_empty() {
            let storage = SurrealStorage::connect(&args.db_path)
                .await
                .with_context(|| format!("failed to open SurrealDB at '{}'", args.db_path))?;
            storage
                .migrate()
                .await
                .context("SurrealDB migration failed")?;
            storage.ping().await.context("SurrealDB ping failed")?;
            info_log(&format!("attached SurrealDB storage at '{}'", args.db_path));
            let storage = Arc::new(storage);

            // Start the query API so CLI tools can route DB ops through hub-server
            // (solves RocksDB single-writer lock contention).
            let api_storage = storage.clone();
            let api_nats = args.nats_url.clone();
            tokio::spawn(async move {
                if let Err(e) =
                    nats_hub::query_api::start_api_listener(api_storage, &api_nats).await
                {
                    eprintln!("[hub-server] query API error: {e}");
                }
            });
            info_log("query API listening on hub.api.>");

            let cp = cp.with_storage(storage);
            return cp.run().await;
        } else {
            info_log("no --db-path provided, running without persistent storage");
        }
    }

    info_log("control plane connected, entering routing loop");
    cp.run().await
}

fn info_log(msg: &str) {
    eprintln!("[hub-server] {msg}");
}
