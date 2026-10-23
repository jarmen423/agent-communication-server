//! hub-server — control plane startup and ordered shutdown.
use anyhow::{Context, Result};
use clap::Parser;
use nats_hub::{ControlPlane, MetricsCollector};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[path = "hub_server/args.rs"]
mod args;
#[path = "hub_server/commands.rs"]
mod commands;
#[path = "hub_server/database.rs"]
mod database;
#[path = "hub_server/metrics.rs"]
mod metrics;
#[path = "hub_server/shutdown.rs"]
mod shutdown;
#[path = "hub_server/storage.rs"]
mod storage;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(EnvFilter::from_default_env().add_directive("nats_hub=info".parse()?))
        .init();
    let args = args::Args::parse();
    match &args.command {
        Some(args::HubCommand::Backup {
            nats_url,
            identity,
            name,
        }) => return commands::run_backup(nats_url, identity.as_deref(), name.as_deref()).await,
        Some(args::HubCommand::Restore { db_path, from }) => {
            return commands::run_restore(db_path, from).await
        }
        None => {}
    }
    shutdown::serve(args).await
}

async fn run_server(
    args: args::Args,
    services: &mut shutdown::Services,
    metrics: Arc<MetricsCollector>,
) -> Result<()> {
    // Set up the live metrics collector (always available, storage-independent).
    if let Some(ref addr) = args.metrics_addr {
        metrics::spawn_metrics_server(addr.clone(), metrics.clone());
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
        let stop = services.intake_stop.clone();
        services.intake.push(tokio::spawn(async move {
            nats_hub::ws_bridge::start_ws_bridge_until(&ws_addr_clone, ws_tx_clone, config, stop)
                .await
        }));
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
    // The hub owns its backup directory: create it, so a fresh data volume
    // (a hosted tenant's container) can take backups without a setup step.
    if let Some(dir) = &args.backup_dir {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("cannot create --backup-dir {}", dir.display()))?;
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

    let (cp, storage) = storage::attach(&args, cp, metrics, api_authz, services).await?;
    let nats = cp.nats_client();
    info_log("control plane connected, entering routing loop");
    cp.run(services.intake_stop.clone()).await?;
    services.finish_intake().await?;
    cp.drain_mirror().await?;
    cp.stop_delivery().await?;
    services.stop_background().await?;
    nats_hub::locks::uninstall();
    drop(cp);
    if let Some(database) = storage {
        database.close().await?;
    }
    shutdown::drain_nats(&nats).await?;
    Ok(())
}

fn info_log(msg: &str) {
    eprintln!("[hub-server] {msg}");
}
