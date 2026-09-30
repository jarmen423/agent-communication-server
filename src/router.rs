//! Control plane router — the brains of nats-hub.
//!
//! Subscribes to `hub.send.>` (everything agents publish) and routes
//! envelopes to the correct `channel.<name>` subjects (see
//! [`route_subject`]). Also listens to `hub.register` / `hub.presence` to
//! track known agents, and mirrors envelopes + registrations to an optional
//! `Storage` backend through a bounded, single-writer queue.

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tracing::{debug, error, info, warn};

use crate::client::{AgentInfo, AgentRegistry};
use crate::protocol::{subjects, Envelope};
use crate::storage::{AgentFilter, AgentRecord, Storage};
use crate::MetricsCollector;

mod mirror;
mod routing;
#[cfg(all(test, feature = "storage-surreal"))]
mod tests;

pub use mirror::DEFAULT_MIRROR_CAPACITY;
pub use routing::{channel_from_send_subject, route_subject, RoutingTable};

use mirror::{MirrorOp, StorageMirror};

/// Router state and per-message logic, independent of the NATS connection
/// (so it can be exercised without a server).
pub(crate) struct RouterCore {
    routing: RoutingTable,
    registry: AgentRegistry,
    metrics: Option<Arc<MetricsCollector>>,
    /// Optional WS bridge broadcast channel. When set, every routed envelope
    /// is serialized to JSON and pushed to all connected visualizer clients.
    ws_event_tx: Option<crate::ws_bridge::EventTx>,
    /// Set once by `ControlPlane::run` when storage is attached.
    mirror: OnceLock<StorageMirror>,
}

impl RouterCore {
    pub(crate) fn new() -> Self {
        Self {
            routing: RoutingTable::new(),
            registry: AgentRegistry::new(),
            metrics: None,
            ws_event_tx: None,
            mirror: OnceLock::new(),
        }
    }

    fn enqueue(&self, op: MirrorOp) {
        if let Some(mirror) = self.mirror.get() {
            mirror.enqueue(op);
        }
    }

    /// Decode an envelope from `hub.send.<channel>`, record metrics, push it
    /// to the WS bridge and decide its destination. `None` = undecodable.
    pub(crate) fn on_send(&self, subject: &str, payload: &[u8]) -> Option<(String, Envelope)> {
        let env = match Envelope::from_json_bytes(payload) {
            Ok(env) => env,
            Err(e) => {
                warn!(%subject, error = %e, "failed to decode envelope, dropping");
                return None;
            }
        };

        // Live metrics: non-blocking atomic increments.
        if let Some(metrics) = &self.metrics {
            metrics.record(&env);
        }
        // Push to WS bridge for visualizer (broadcast::send is non-blocking).
        if let Some(tx) = &self.ws_event_tx {
            if let Ok(json) = serde_json::to_string(&env) {
                let _ = tx.send(json);
            }
        }

        let dest = route_subject(subject, &env);
        debug!(
            id = %env.meta.id,
            from = %env.meta.from,
            to = ?env.meta.to,
            kind = ?env.meta.kind,
            %dest,
            "routing envelope"
        );
        Some((dest, env))
    }

    /// Queue a routed envelope for the storage mirror (non-blocking).
    pub(crate) fn mirror_envelope(&self, env: Envelope) {
        self.enqueue(MirrorOp::Envelope(Box::new(env)));
    }

    /// `hub.register`: record identity + capabilities (replacing any
    /// previous capabilities) and persist the registration.
    pub(crate) async fn on_register(&self, payload: &[u8]) {
        let env = match Envelope::from_json_bytes(payload) {
            Ok(env) => env,
            Err(e) => {
                warn!(error = %e, "failed to decode registration envelope");
                return;
            }
        };
        let Some(ident) = env.payload.get("identity").and_then(|v| v.as_str()) else {
            return;
        };
        let caps: Vec<String> = env
            .payload
            .get("capabilities")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        info!(identity = %ident, ?caps, "agent registered");
        self.registry
            .register(ident.to_string(), caps.clone())
            .await;
        self.routing.set_capabilities(ident, &caps).await;

        let now = chrono::Utc::now();
        self.enqueue(MirrorOp::Register(AgentRecord {
            identity: ident.to_string(),
            capabilities: caps,
            last_seen: now,
            registered_at: now,
            metadata: serde_json::json!({}),
        }));
    }

    /// `hub.presence`: refresh liveness only. Capabilities are preserved
    /// (an unknown agent is added with none until it registers).
    pub(crate) async fn on_presence(&self, payload: &[u8]) {
        let env = match Envelope::from_json_bytes(payload) {
            Ok(env) => env,
            Err(e) => {
                warn!(error = %e, "failed to decode presence envelope");
                return;
            }
        };
        let Some(ident) = env.payload.get("identity").and_then(|v| v.as_str()) else {
            return;
        };
        debug!(identity = %ident, "heartbeat received");
        self.registry.touch(ident).await;
        self.enqueue(MirrorOp::Touch(ident.to_string()));
    }
}

/// The control plane worker. Runs an event loop that:
/// 1. Subscribes to `hub.send.>` — all outgoing agent messages.
/// 2. Routes each envelope with [`route_subject`].
/// 3. Subscribes to `hub.register` — tracks agent registrations.
/// 4. Subscribes to `hub.presence` — tracks heartbeats.
/// 5. Mirrors envelopes + registrations to `Storage` via a bounded queue.
pub struct ControlPlane {
    nats: async_nats::Client,
    core: RouterCore,
    storage: Option<Arc<dyn Storage>>,
    mirror_capacity: usize,
}

impl ControlPlane {
    /// Create a new control plane connected to the given NATS URL.
    ///
    /// Auth/TLS follow [`crate::HubConnectOptions::from_env`] (`NATS_TOKEN`,
    /// `NATS_USER`/`NATS_PASSWORD`, `NATS_CREDENTIALS_FILE`, `NATS_REQUIRE_TLS`, …)
    /// so hub-server can join a token-gated or TLS hub without a custom flag surface.
    pub async fn connect(url: &str) -> Result<Self> {
        let opts = crate::HubConnectOptions::from_env();
        let nats = crate::connect_opts::connect_with_hub_opts(url, &opts)
            .await
            .with_context(|| format!("control plane: connect to {url} failed"))?;
        Ok(Self {
            nats,
            core: RouterCore::new(),
            storage: None,
            mirror_capacity: DEFAULT_MIRROR_CAPACITY,
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

        let mut send_sub = self
            .nats
            .subscribe(format!("{}.>", subjects::SEND_PREFIX))
            .await
            .context("control plane: subscribe to hub.send.> failed")?;
        let mut reg_sub = self
            .nats
            .subscribe(subjects::REGISTER)
            .await
            .context("control plane: subscribe to hub.register failed")?;
        let mut presence_sub = self
            .nats
            .subscribe(subjects::PRESENCE)
            .await
            .context("control plane: subscribe to hub.presence failed")?;

        info!("control plane subscribed to hub.send.>, hub.register, hub.presence");

        loop {
            tokio::select! {
                Some(msg) = send_sub.next() => self.handle_send(&msg).await,
                Some(msg) = reg_sub.next() => self.core.on_register(&msg.payload).await,
                Some(msg) = presence_sub.next() => self.core.on_presence(&msg.payload).await,
            }
        }
    }

    /// Route one `hub.send.<channel>` message. No per-message `flush()`:
    /// async-nats batches and flushes outgoing publishes on its own, so the
    /// loop never waits on a network round-trip.
    async fn handle_send(&self, msg: &async_nats::Message) {
        let Some((dest, env)) = self.core.on_send(msg.subject.as_str(), &msg.payload) else {
            return;
        };
        if let Err(e) = self.nats.publish(dest.clone(), msg.payload.clone()).await {
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
