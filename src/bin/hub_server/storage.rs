use super::{args::Args, database::Database, info_log, shutdown::Services};
use anyhow::{Context, Result};
use nats_hub::{ControlPlane, MetricsCollector, Storage};
use std::sync::Arc;

pub(super) async fn attach(
    args: &Args,
    cp: ControlPlane,
    metrics: Arc<MetricsCollector>,
    api_authz: nats_hub::query_api::ApiAuthz,
    services: &mut Services,
) -> Result<(ControlPlane, Option<Database>)> {
    if !args.db_path.is_empty() {
        let database = Database::open(&args.db_path)
            .await
            .with_context(|| format!("failed to open SurrealDB at '{}'", args.db_path))?;
        let storage = database.storage.clone();
        storage
            .migrate()
            .await
            .context("SurrealDB migration failed")?;
        storage.ping().await.context("SurrealDB ping failed")?;
        info_log(&format!("attached SurrealDB storage at '{}'", args.db_path));

        // Start the query API so CLI tools can route DB ops through hub-server
        // (solves RocksDB single-writer lock contention).
        let api_storage = storage.clone();
        let api_nats = cp.nats_client();
        let stop = services.intake_stop.clone();
        services.intake.push(tokio::spawn(async move {
            nats_hub::query_api::serve_api_listener(api_storage, api_nats, api_authz, stop).await
        }));
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
            services.background_stop.clone(),
        )
        .await
        {
            Ok((_, task)) => {
                services.background.push(task);
                info_log(&format!(
                    "wave orchestrator running (worker liveness TTL {liveness_secs}s)"
                ));
            }
            Err(e) => eprintln!(
                "[hub-server] wave orchestrator failed to start: {e:#} \
                     (wave.spawn/cancel ops will return errors)"
            ),
        }

        // File-lock service: owns lock.* writes, presence expiry and
        // lock events on channel.lock.<scope-hash> (docs/FILE_LOCKS.md).
        let lock_liveness = args
            .lock_liveness_secs
            .or_else(|| {
                std::env::var("NATS_HUB_LOCK_LIVENESS_SECS")
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(nats_hub::locks::DEFAULT_LOCK_LIVENESS_SECS);
        let lock_config = nats_hub::locks::LockConfig {
            liveness_ttl: std::time::Duration::from_secs(lock_liveness),
            require_bound_identity: args.require_bound_identity,
        };
        match nats_hub::locks::LockService::start(
            storage.clone(),
            &args.nats_url,
            lock_config,
            services.background_stop.clone(),
        )
        .await
        {
            Ok((_, task)) => {
                services.background.push(task);
                info_log(&format!(
                    "file-lock service running (holder liveness TTL {lock_liveness}s)"
                ));
            }
            Err(e) => eprintln!(
                "[hub-server] lock service failed to start: {e:#} \
                     (lock.* ops will return errors)"
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
            interval: std::time::Duration::from_secs(args.retention_interval_secs.unwrap_or(3600)),
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
            services
                .background
                .push(nats_hub::storage::retention::spawn(
                    storage.clone(),
                    policy,
                    metrics.clone(),
                    services.background_stop.clone(),
                ));
        }

        let cp = cp.with_storage(storage.clone());
        return Ok((cp, Some(database)));
    } else {
        info_log("no --db-path provided, running without persistent storage");
    }

    Ok((cp, None))
}
