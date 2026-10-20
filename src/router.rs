//! Control plane router — the brains of nats-hub.
//!
//! Subscribes to `hub.send.>` (everything agents publish) and routes
//! envelopes to the correct `channel.<name>` subjects (see
//! [`route_subject`]). Also listens to `hub.register` / `hub.presence` to
//! track known agents, and mirrors envelopes + registrations to an optional
//! `Storage` backend through a bounded, single-writer queue.

use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tracing::{debug, error, info, warn};

use crate::client::AgentInfo;
use crate::delivery::DeliveryConfig;
use crate::protocol::subjects;
use crate::storage::{AgentFilter, AgentRecord, Storage};
use crate::MetricsCollector;

mod durable;
mod mirror;
mod routing;
mod state;
#[cfg(all(test, feature = "storage-surreal"))]
mod tests;

pub use mirror::DEFAULT_MIRROR_CAPACITY;
pub use routing::{
    bound_identity_subject, bound_send_subject, channel_from_send_subject, route_channel,
    route_subject, RoutingTable,
};

use mirror::StorageMirror;
pub(crate) use state::RouterCore;

/// Connect options for hub-server's own connections (router, query API):
/// env auth, with the reply inbox defaulting to `_INBOX.hub-server`
/// (contract C1) when neither `NATS_USER` nor `NATS_HUB_IDENTITY` names one.
pub(crate) fn hub_server_connect_opts() -> crate::HubConnectOptions {
    let mut opts = crate::HubConnectOptions::from_env();
    if opts.inbox_identity.is_none() {
        opts.inbox_identity = Some(HUB_SERVER_IDENTITY.to_string());
    }
    opts
}

/// Identity of hub-server's own NATS service user (reserved; see
/// [`crate::protocol::RESERVED_IDENTITIES`]).
pub const HUB_SERVER_IDENTITY: &str = "hub-server";

/// The control plane worker. Runs an event loop that:
/// 1. Subscribes to `hub.send.>` + `hub.pub.*.>` — all outgoing agent
///    messages (legacy + bound subjects; contract §4.1).
/// 2. Routes each envelope with [`route_subject`]; on bound subjects the
///    `<identity>` token overwrites `meta.from` before delivery.
/// 3. Subscribes to `hub.register` + `hub.register.*` — agent registrations.
/// 4. Subscribes to `hub.presence` + `hub.presence.*` — heartbeats.
/// 5. Mirrors envelopes + registrations to `Storage` via a bounded queue.
pub struct ControlPlane {
    nats: async_nats::Client,
    core: RouterCore,
    storage: Option<Arc<dyn Storage>>,
    mirror_capacity: usize,
    /// `Some` = route from the JetStream ingress stream (`--durable`).
    durable: Option<Arc<DeliveryConfig>>,
}

impl ControlPlane {
    /// Create a new control plane connected to the given NATS URL.
    ///
    /// Auth/TLS follow [`crate::HubConnectOptions::from_env`] (`NATS_TOKEN`,
    /// `NATS_USER`/`NATS_PASSWORD`, `NATS_CREDENTIALS_FILE`, `NATS_REQUIRE_TLS`, …)
    /// so hub-server can join a token-gated or TLS hub without a custom flag surface.
    pub async fn connect(url: &str) -> Result<Self> {
        let opts = hub_server_connect_opts();
        let nats = crate::connect_opts::connect_with_hub_opts(url, &opts)
            .await
            .with_context(|| format!("control plane: connect to {url} failed"))?;
        Ok(Self {
            nats,
            core: RouterCore::new(),
            storage: None,
            mirror_capacity: DEFAULT_MIRROR_CAPACITY,
            durable: None,
        })
    }

    /// Attach a live metrics collector. Records one atomic increment per
    /// routed envelope on the hot path. Safe to call before `run()`.
    pub fn with_metrics(mut self, metrics: Arc<MetricsCollector>) -> Self {
        self.core.metrics = Some(metrics);
        self
    }

    /// Attach a WS bridge broadcast channel for the visualizer.
    /// Every routed envelope is serialized to JSON and pushed here.
    pub fn with_ws_events(mut self, tx: crate::ws_bridge::EventTx) -> Self {
        self.core.ws_event_tx = Some(tx);
        self
    }

    /// When set, the router drops messages on legacy subjects (`hub.send.>`,
    /// bare `hub.register`, bare `hub.presence`) — every sender must use the
    /// bound subjects that pin `meta.from` to the connection identity
    /// (contract §4.1). Default off during the migration window.
    pub fn with_require_bound_identity(mut self, require: bool) -> Self {
        self.core.require_bound = require;
        self
    }

    /// Attach a storage backend for persistence (agent registry, message
    /// history, conversation threading). Must be called before `run()`.
    pub fn with_storage(mut self, storage: Arc<dyn Storage>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Capacity of the bounded storage-mirror queue (default
    /// [`DEFAULT_MIRROR_CAPACITY`]). Writes beyond it are dropped and counted.
    pub fn with_mirror_capacity(mut self, capacity: usize) -> Self {
        self.mirror_capacity = capacity;
        self
    }

    /// Durable delivery (`hub-server --durable`): route from the JetStream
    /// ingress stream through the shared `router` consumer and store DMs and
    /// task/session/wave traffic (see [`crate::delivery`]). Without it the
    /// router uses plain core-NATS subscriptions, exactly as before.
    pub fn with_durable(mut self, cfg: DeliveryConfig) -> Self {
        self.durable = Some(Arc::new(cfg));
        self
    }

    /// Load all known agents from the attached `Storage` backend into the
    /// in-memory `AgentRegistry`. The in-memory registry acts as a hot-path
    /// cache; the DB is the source of truth across restarts.
    ///
    /// Returns the number of agents loaded. If no storage is attached,
    /// this is a no-op that returns 0.
    pub async fn load_agents_from_storage(&self) -> Result<usize> {
        let Some(storage) = &self.storage else {
            debug!("load_agents_from_storage: no storage attached, skipping");
            return Ok(0);
        };

        let records: Vec<AgentRecord> = storage.find_agents(&AgentFilter::new()).await?;
        let count = records.len();

        for r in records {
            self.core
                .registry
                .register(r.identity.clone(), r.capabilities.clone())
                .await;
            // register() stamps last_seen = now; restore the DB-recorded
            // value so the real liveness signal survives a restart.
            self.core
                .registry
                .force_last_seen(&r.identity, r.last_seen)
                .await;
            self.core
                .routing
                .set_capabilities(&r.identity, &r.capabilities)
                .await;
        }

        info!(count, "loaded agents from storage into in-memory registry");
        Ok(count)
    }

    /// Run the control plane event loop. Blocks until cancelled.
    pub async fn run(&self) -> Result<()> {
        info!("control plane router starting");

        // 0. Start the storage mirror writer and warm the in-memory cache
        //    from storage. A failure to load is non-fatal: fresh
        //    registrations/heartbeats repopulate the registry.
        if let Some(storage) = &self.storage {
            let mirror = self.core.mirror.get_or_init(|| {
                StorageMirror::new(
                    storage.clone(),
                    self.mirror_capacity,
                    self.core.metrics.clone(),
                )
            });
            mirror.start().await;
            if let Err(e) = self.load_agents_from_storage().await {
                warn!(error = %e, "failed to warm in-memory registry from storage (non-fatal)");
            }
        }

        if let Some(cfg) = self.durable.clone() {
            return self.run_durable(cfg).await;
        }

        let mut send_sub = self
            .nats
            .subscribe(format!("{}.>", subjects::SEND_PREFIX))
            .await
            .context("control plane: subscribe to hub.send.> failed")?;
        let mut bound_send_sub = self
            .nats
            .subscribe(format!("{}.*.>", subjects::PUB_PREFIX))
            .await
            .context("control plane: subscribe to hub.pub.*.> failed")?;
        let mut feeds = self.subscribe_feeds().await?;

        info!(
            require_bound = self.core.require_bound,
            "control plane subscribed to hub.send.>, hub.pub.*.>, hub.register(.*), hub.presence(.*)"
        );

        loop {
            tokio::select! {
                Some(msg) = send_sub.next() => self.handle_send(&msg).await,
                Some(msg) = bound_send_sub.next() => self.handle_send(&msg).await,
                Some(msg) = feeds.next() => self.on_feed(&msg).await,
            }
        }
    }

    /// `--durable`: route from the shared ingress consumer; registrations
    /// and heartbeats stay core-NATS (they are repeated, not stored).
    async fn run_durable(&self, cfg: Arc<DeliveryConfig>) -> Result<()> {
        let ingress = self.start_durable(cfg).await?;
        let mut feeds = self.subscribe_feeds().await?;
        let mut msgs = self.ingress_messages(&ingress).await?;
        info!(
            require_bound = self.core.require_bound,
            "control plane routing from the durable ingress consumer; subscribed to hub.register(.*), hub.presence(.*)"
        );
        loop {
            tokio::select! {
                item = msgs.next() => match item {
                    Some(Ok(msg)) => self.handle_durable(&ingress, msg).await,
                    Some(Err(e)) => debug!(error = %e, "ingress consumer stream error"),
                    None => {
                        warn!("ingress consumer stream ended; re-opening");
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        match self.ingress_messages(&ingress).await {
                            Ok(m) => msgs = m,
                            Err(e) => error!(error = %e, "re-opening the ingress consumer failed"),
                        }
                    }
                },
                Some(msg) = feeds.next() => self.on_feed(&msg).await,
            }
        }
    }

    /// Registration + presence subscriptions (legacy and bound), merged.
    async fn subscribe_feeds(
        &self,
    ) -> Result<futures_util::stream::SelectAll<async_nats::Subscriber>> {
        let mut subs = Vec::new();
        for subject in [
            subjects::REGISTER.to_string(),
            format!("{}.*", subjects::REGISTER),
            subjects::PRESENCE.to_string(),
            format!("{}.*", subjects::PRESENCE),
        ] {
            subs.push(
                self.nats
                    .subscribe(subject.clone())
                    .await
                    .with_context(|| format!("control plane: subscribe to {subject} failed"))?,
            );
        }
        Ok(futures_util::stream::select_all(subs))
    }

    /// One registration or heartbeat message.
    async fn on_feed(&self, msg: &async_nats::Message) {
        let subject = msg.subject.as_str();
        if subject == subjects::REGISTER || subject.starts_with("hub.register.") {
            self.core.on_register(subject, &msg.payload).await;
        } else {
            self.core.on_presence(subject, &msg.payload).await;
        }
    }

    /// Route one `hub.send.<channel>` / `hub.pub.<id>.<channel>` message.
    /// No per-message `flush()`: async-nats batches and flushes outgoing
    /// publishes on its own, so the loop never waits on a network round-trip.
    async fn handle_send(&self, msg: &async_nats::Message) {
        let Some((dest, env, bytes)) = self.core.on_send(msg.subject.as_str(), &msg.payload) else {
            return;
        };
        if let Err(e) = self.nats.publish(dest.clone(), bytes.into()).await {
            error!(%dest, error = %e, "failed to route envelope");
        }
        self.core.mirror_envelope(env);
    }

    /// Get a snapshot of known agents (in-memory registry).
    pub async fn known_agents(&self) -> Vec<AgentInfo> {
        self.core.registry.list().await
    }

    /// The capability routing table (informational; see [`RoutingTable`]).
    pub fn routing_table(&self) -> &RoutingTable {
        &self.core.routing
    }

    /// Storage-mirror writes dropped because the queue was full.
    pub fn mirror_dropped(&self) -> u64 {
        self.core.mirror.get().map_or(0, StorageMirror::dropped)
    }
}
