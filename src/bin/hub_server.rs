//! hub-server — runs the control plane router.
//!
//! Usage: hub-server [--nats-url URL] [--db-path PATH] [--durable [--delivery-config PATH]]
//!
//! Start the NATS server first:  nats-server -c config/nats-server.conf
//! Then:                        cargo run --bin hub-server
//!
//! If `--db-path` is provided, the server attaches a SurrealDB-backed
//! `Storage` for persistent agent registry and message history. The DB
//! file is created on first run; subsequent runs warm the in-memory
//! agent cache from the persisted copy.

use std::io::{Read, Write};
use std::net::TcpListener;
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
    /// Loopback spellings (localhost/127.0.0.1/[::1]) are allowed by
    /// default; entries are normalized — case, trailing `/` and default
    /// ports don't matter.
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

    /// Drop messages on the legacy self-asserted subjects (`hub.send.>`,
    /// bare `hub.register`/`hub.presence`, `hub.api.<op>`). Every sender
    /// must use the bound subjects that pin identity into the subject
    /// (contract §4.1). Default off during the migration window.
    #[arg(long)]
    require_bound_identity: bool,

    /// Durable delivery over JetStream: sends survive a router restart,
    /// DMs wait for offline agents, task channels are kept for resume, and
    /// several hub-servers share one routing consumer. Needs a nats-server
    /// with JetStream enabled. Off = plain core NATS (see docs/DELIVERY.md).
    #[arg(long)]
    durable: bool,

    /// JSON file with stream names/limits for --durable (any subset of the
    /// fields in docs/DELIVERY.md). Falls back to env NATS_HUB_DELIVERY_CONFIG.
    #[arg(long, value_name = "PATH", requires = "durable")]
    delivery_config: Option<std::path::PathBuf>,

    /// Identity allowed to call query-API write ops (wave.*/session.*
    /// mutations). Repeatable; env `NATS_HUB_API_ADMINS` (comma-separated)
    /// is merged in.
    #[arg(long = "api-admin")]
    api_admin: Vec<String>,

    /// Directory the `backup.export` op (and `hub-server backup`) writes
    /// into. Without it the op is disabled. Callers only pick a file name.
    #[arg(long)]
    backup_dir: Option<std::path::PathBuf>,

    /// Worker liveness TTL (seconds) for wave orchestration: a worker
    /// silent longer than this loses its running wave tasks. Falls back
    /// to env NATS_HUB_WAVE_LIVENESS_SECS, then 90.
    #[cfg(feature = "storage-surreal")]
    #[arg(long)]
    wave_liveness_secs: Option<u64>,

    /// Envelope retention: prune history older than N days, in bounded
    /// batches from a background task. Off by default. Envelopes on
    /// channels a live wave/session still uses are never pruned.
    #[cfg(feature = "storage-surreal")]
    #[arg(long)]
    retention_days: Option<u64>,

    /// Envelope retention: cap total stored envelopes at N (oldest
    /// unprotected rows go first). Off by default.
    #[cfg(feature = "storage-surreal")]
    #[arg(long)]
    retention_max_envelopes: Option<u64>,

    /// How long terminal (closed/failed/cancelled) waves and sessions are
    /// kept, in days — record plus all of their channels' envelopes.
    /// Defaults to max(4 * --retention-days, 30) when --retention-days is
    /// set; terminal state is never pruned otherwise.
    #[cfg(feature = "storage-surreal")]
    #[arg(long)]
    retention_terminal_days: Option<u64>,

    /// Max rows deleted per retention statement. Default 1000.
    #[cfg(feature = "storage-surreal")]
    #[arg(long)]
    retention_batch_size: Option<usize>,

    /// Seconds between retention passes. Default 3600.
    #[cfg(feature = "storage-surreal")]
    #[arg(long)]
    retention_interval_secs: Option<u64>,

    /// Operator subcommands. Bare `hub-server` (no subcommand) runs the
    /// router — all flags above apply to that mode.
    #[command(subcommand)]
    command: Option<HubCommand>,
}

#[derive(clap::Subcommand)]
enum HubCommand {
    /// Write a consistent backup of the RUNNING hub-server's database by
    /// calling the admin-only `backup.export` query-API op. The export
    /// lands on the hub host as <--backup-dir of that hub-server>/<--name>
    /// (the file must not already exist).
    Backup {
        /// NATS server URL
        #[arg(long, default_value = "nats://127.0.0.1:4222")]
        nats_url: String,

        /// Caller identity for the bound api subject (defaults to
        /// NATS_HUB_IDENTITY). Must be in the hub's --api-admin list when
        /// API authorization is enforced.
        #[arg(long)]
        identity: Option<String>,

        /// File name inside the running hub-server's --backup-dir
        /// ([A-Za-z0-9._-], no '/'). Default: hub-<UTC timestamp>.surql
        #[arg(long)]
        name: Option<String>,
    },

    /// Restore a `hub-server backup` export into a FRESH --db-path.
    /// Offline: the hub-server owning that path must be stopped first
    /// (RocksDB is single-writer), and the path must be empty.
    #[cfg(feature = "storage-surreal")]
    Restore {
        /// Fresh SurrealDB path to restore into (must not exist or be empty)
        #[arg(long)]
        db_path: String,

        /// The .surql export file produced by `hub-server backup`
        #[arg(long)]
        from: std::path::PathBuf,
    },
}

/// `hub-server backup`: ask the running hub-server to export its DB.
async fn run_backup(nats_url: &str, identity: Option<&str>, name: Option<&str>) -> Result<()> {
    let name = name
        .map(String::from)
        .unwrap_or_else(|| format!("hub-{}.surql", chrono::Utc::now().format("%Y%m%dT%H%M%SZ")));
    let api = match identity {
        Some(id) => nats_hub::ApiClient::connect_with_identity(nats_url, id).await?,
        None => nats_hub::ApiClient::connect(nats_url).await?,
    };
    let resp = api
        .request("backup.export", serde_json::json!({ "name": name }))
        .await?;
    let path = resp.get("path").and_then(|v| v.as_str()).unwrap_or("?");
    let bytes = resp.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0);
    println!("backup written: {path} ({bytes} bytes)");
    Ok(())
}

/// `hub-server restore`: import an export into a fresh DB path. The file
/// carries its own schema (`DEFINE TABLE`/`FIELD`/`INDEX`), so the path
/// must be fresh — then `migrate()` runs to normalize whatever the export
/// predates.
#[cfg(feature = "storage-surreal")]
async fn run_restore(db_path: &str, from: &std::path::Path) -> Result<()> {
    let p = std::path::Path::new(db_path);
    let non_empty = p.is_file()
        || (p.is_dir()
            && std::fs::read_dir(p)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false));
    anyhow::ensure!(
        !non_empty,
        "--db-path '{db_path}' is not empty; restore requires a fresh path"
    );
    let storage = SurrealStorage::connect(db_path)
        .await
        .with_context(|| format!("failed to open SurrealDB at '{db_path}'"))?;
    nats_hub::storage::backup::import_from(&storage.db(), from).await?;
    storage
        .migrate()
        .await
        .context("post-restore migration failed")?;
    let counts = nats_hub::storage::backup::table_counts(&storage.db()).await?;
    println!("restored '{}' into '{db_path}':", from.display());
    for (table, n) in counts {
        println!("  {table:<12} {n}");
    }
    Ok(())
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

    // Operator subcommands run and exit — the rest of main is the router.
    match args.command {
        Some(HubCommand::Backup {
            ref nats_url,
            ref identity,
            ref name,
        }) => return run_backup(nats_url, identity.as_deref(), name.as_deref()).await,
        #[cfg(feature = "storage-surreal")]
        Some(HubCommand::Restore {
            ref db_path,
            ref from,
        }) => return run_restore(db_path, from).await,
        _ => {}
    }

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
        // The bridge connects and publishes as this identity, so it must be
        // a valid one; fail at startup rather than in the retry loop.
        nats_hub::protocol::require_valid_identity(&identity).context("--ws-identity")?;
        // `visualizer` is the bridge's own reserved name; the router and
        // orchestrator identities must never be reachable from a browser.
        if identity != "visualizer" && nats_hub::protocol::reserved_identity(&identity) {
            anyhow::bail!("--ws-identity '{identity}' is reserved for a hub component");
        }

        // A tokenless bridge may only bind loopback.
        nats_hub::ws_bridge::check_ws_bind(ws_addr, token.as_deref(), args.ws_insecure)?;

        let mut allowed_origins = nats_hub::ws_bridge::default_allowed_origins(ws_addr);
        allowed_origins.extend(args.ws_allow_origin.iter().cloned());
        if !nats_hub::ws_bridge::is_loopback_addr(ws_addr) && args.ws_allow_origin.is_empty() {
            info_log(
                "hint: non-loopback --ws-addr — remote browsers need their Origin in \
                 --ws-allow-origin (e.g. --ws-allow-origin http://<lan-ip>:<port>)",
            );
        }

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
                "WS bridge (visualizer) on http://{ws_addr}/?token={}",
                nats_hub::ws_bridge::url_query_encode(t)
            )),
            None => {
                tracing::warn!(
                    "WS bridge on {ws_addr} is running UNAUTHENTICATED — no --ws-token/HUB_WS_TOKEN (safe only on loopback or a trusted network)"
                );
                let suffix = if nats_hub::ws_bridge::is_loopback_addr(ws_addr) {
                    " (no token; loopback only)"
                } else {
                    " (no token — running unauthenticated)"
                };
                info_log(&format!(
                    "WS bridge (visualizer) on http://{ws_addr}{suffix}"
                ));
            }
        }
    }

    // Query-API authorization: --api-admin flags + NATS_HUB_API_ADMINS env.
    let mut api_admins: std::collections::BTreeSet<String> =
        args.api_admin.iter().cloned().collect();
    if let Ok(env_admins) = std::env::var("NATS_HUB_API_ADMINS") {
        api_admins.extend(
            env_admins
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from),
        );
    }
    let api_authz = nats_hub::query_api::ApiAuthz {
        require_bound: args.require_bound_identity,
        admins: api_admins,
        backup_dir: args.backup_dir.clone(),
    };
    if args.require_bound_identity {
        info_log("require-bound-identity: legacy hub.send/hub.register/hub.presence/hub.api.<op> subjects are rejected");
    }

    let cp = ControlPlane::connect(&args.nats_url)
        .await
        .context("failed to connect to NATS")?;
    let mut cp = cp
        .with_metrics(metrics.clone())
        .with_ws_events(ws_tx)
        .with_require_bound_identity(args.require_bound_identity);
    if args.durable {
        let path = args.delivery_config.clone().or_else(|| {
            std::env::var_os("NATS_HUB_DELIVERY_CONFIG")
                .filter(|v| !v.is_empty())
                .map(std::path::PathBuf::from)
        });
        let cfg = match &path {
            Some(p) => nats_hub::delivery::DeliveryConfig::from_file(p)?,
            None => nats_hub::delivery::DeliveryConfig::default(),
        };
        info_log(&format!(
            "durable delivery: streams {}/{}/{} (config: {})",
            cfg.ingress.name,
            cfg.inbox.name,
            cfg.tasks.name,
            path.as_ref()
                .map_or("defaults".to_string(), |p| p.display().to_string())
        ));
        cp = cp.with_durable(cfg);
    }

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
                if let Err(e) = nats_hub::query_api::start_api_listener_with_authz(
                    api_storage,
                    &api_nats,
                    api_authz,
                )
                .await
                {
                    eprintln!("[hub-server] query API error: {e}");
                }
            });
            info_log("query API listening on hub.api.>");

            // Wave orchestration lives in hub-server: it owns wave state
            // machines, resumes running waves after a restart, and enforces
            // worker liveness for wave tasks.
            let liveness_secs = args
                .wave_liveness_secs
                .or_else(|| {
                    std::env::var("NATS_HUB_WAVE_LIVENESS_SECS")
                        .ok()
                        .and_then(|v| v.parse().ok())
                })
                .unwrap_or(nats_hub::orchestrator::DEFAULT_LIVENESS_SECS);
            let orch_config = nats_hub::orchestrator::OrchestratorConfig {
                liveness_ttl: std::time::Duration::from_secs(liveness_secs),
                require_bound_identity: args.require_bound_identity,
            };
            match nats_hub::orchestrator::WaveOrchestrator::start(
                storage.clone(),
                &args.nats_url,
                orch_config,
            )
            .await
            {
                Ok(_) => info_log(&format!(
                    "wave orchestrator running (worker liveness TTL {liveness_secs}s)"
                )),
                Err(e) => eprintln!(
                    "[hub-server] wave orchestrator failed to start: {e:#} \
                     (wave.spawn/cancel ops will return errors)"
                ),
            }

            // Retention pruner: a background task deleting in bounded
            // batches; off unless a --retention-* rule is configured.
            let days = |d: u64| std::time::Duration::from_secs(d.saturating_mul(86_400));
            let policy = nats_hub::storage::retention::RetentionPolicy {
                max_envelope_age: args.retention_days.map(days),
                max_envelopes: args.retention_max_envelopes,
                terminal_retention: args
                    .retention_terminal_days
                    .or_else(|| args.retention_days.map(|d| (d.saturating_mul(4)).max(30)))
                    .map(days),
                batch_size: args
                    .retention_batch_size
                    .unwrap_or(nats_hub::storage::retention::RetentionPolicy::DEFAULT_BATCH),
                interval: std::time::Duration::from_secs(
                    args.retention_interval_secs.unwrap_or(3600),
                ),
            };
            if policy.enabled() {
                info_log(&format!(
                    "retention: age {:?}, cap {:?}, terminal {:?}, batch {}, every {:?}",
                    policy.max_envelope_age,
                    policy.max_envelopes,
                    policy.terminal_retention,
                    policy.batch_size,
                    policy.interval
                ));
                nats_hub::storage::retention::spawn(storage.clone(), policy, metrics.clone());
            }

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
