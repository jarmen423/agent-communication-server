//! The orchestrator event loop: wave subscriptions, worker liveness,
//! persisted task transitions, dependency dispatch, and restart resume.
//!
//! The loop itself lives here; the handlers are split by concern:
//! [`super::commands`] (API ops), [`super::inbound`] (wave-channel
//! envelopes), [`super::transitions`] (task state changes),
//! [`super::dispatch`] (dispatch + wave finalization), [`super::liveness`]
//! and [`super::resume`].

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::client::HubClient;
use crate::events::event_payload;
use crate::protocol::{subjects, Envelope, MessageKind};
use crate::storage::{Storage, WaveTaskRecord};

use super::api::{install_handle, OrchCommand, OrchestratorHandle};

/// How long resume waits for the router to route the readiness probe.
const ROUTER_READY_TIMEOUT: Duration = Duration::from_secs(15);
/// Re-send interval of the readiness probe.
const ROUTER_PROBE_INTERVAL: Duration = Duration::from_millis(250);
/// Channel of the readiness probe DM (routed to our own inbox).
const ROUTER_PROBE_CHANNEL: &str = "orchestrator.ready";
use super::state::{OrchestratorConfig, WaveRt, ORCHESTRATOR_IDENTITY};

pub struct WaveOrchestrator {
    pub(super) storage: Arc<dyn Storage>,
    pub(super) client: HubClient,
    pub(super) config: OrchestratorConfig,
    pub(super) cmd_rx: mpsc::Receiver<OrchCommand>,
    pub(super) waves: HashMap<String, WaveRt>,
    /// identity → most recent proof of life (heartbeat or wave-channel
    /// envelope carrying that `meta.from`).
    pub(super) last_seen: HashMap<String, Instant>,
}

impl WaveOrchestrator {
    /// Connect to NATS, install the process-wide API handle, and spawn the
    /// run loop. Returns the command handle (also via `orchestrator_handle()`).
    pub async fn start(
        storage: Arc<dyn Storage>,
        nats_url: &str,
        config: OrchestratorConfig,
    ) -> Result<OrchestratorHandle> {
        let client = HubClient::connect(nats_url, ORCHESTRATOR_IDENTITY)
            .await
            .context("orchestrator: NATS connect failed")?;
        let (tx, rx) = mpsc::channel(32);
        let mut orch = Self {
            storage,
            client,
            config,
            cmd_rx: rx,
            waves: HashMap::new(),
            last_seen: HashMap::new(),
        };
        install_handle(tx.clone());
        tokio::spawn(async move {
            if let Err(e) = orch.run().await {
                warn!(error = %e, "wave orchestrator exited");
            }
        });
        Ok(tx)
    }

    async fn run(&mut self) -> Result<()> {
        let mut wave_rx = self
            .client
            .subscribe_subject("channel.wave.>")
            .await
            .context("orchestrator: subscribe channel.wave.> failed")?;
        // Heartbeats arrive on the legacy `hub.presence` and on the bound
        // `hub.presence.<identity>` (contract §4.1, what workers send since
        // T1). Missing the bound form would fail live long-running tasks as
        // "silent" after the liveness TTL.
        let mut presence_rx = self
            .client
            .subscribe_subject_tagged(subjects::PRESENCE)
            .await
            .context("orchestrator: subscribe hub.presence failed")?;
        let mut bound_presence_rx = self
            .client
            .subscribe_subject_tagged(&format!("{}.*", subjects::PRESENCE))
            .await
            .context("orchestrator: subscribe hub.presence.* failed")?;

        // Resume re-dispatches through the router; wait until it routes.
        self.wait_for_router().await;
        if let Err(e) = self.resume().await {
            warn!(error = %e, "wave resume failed; starting with an empty slate");
        }

        // Sweep often enough to catch a dead worker near its TTL.
        let period =
            (self.config.liveness_ttl / 3).clamp(Duration::from_secs(1), Duration::from_secs(10));
        let mut sweep = tokio::time::interval(period);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        info!("wave orchestrator running");
        loop {
            tokio::select! {
                Some(env) = wave_rx.recv() => self.on_wave_envelope(env).await,
                Some((subj, env)) = presence_rx.recv() => self.on_presence(&subj, &env),
                Some((subj, env)) = bound_presence_rx.recv() => self.on_presence(&subj, &env),
                Some(cmd) = self.cmd_rx.recv() => self.on_command(cmd).await,
                _ = sweep.tick() => self.sweep().await,
            }
        }
    }

    /// Block until the control-plane router routes our DMs (it starts after
    /// the orchestrator in hub-server and subscribes to `hub.pub.*.>` only
    /// once its storage warm-up is done). A `session_start` re-sent before
    /// that would be dropped by NATS. We DM ourselves a probe until it comes
    /// back on our inbox; after [`ROUTER_READY_TIMEOUT`] we resume anyway.
    async fn wait_for_router(&self) {
        let identity = self.client.identity().to_string();
        let mut inbox = match self.client.subscribe_inbox().await {
            Ok(rx) => rx,
            Err(e) => {
                warn!(error = %e, "router probe: inbox subscribe failed; resuming without it");
                return;
            }
        };
        let deadline = Instant::now() + ROUTER_READY_TIMEOUT;
        for attempt in 1u32.. {
            let probe = Envelope::new(
                &identity,
                ROUTER_PROBE_CHANNEL,
                MessageKind::Control,
                json!({"action": "router_probe"}),
            )
            .to(&identity);
            if let Err(e) = self.client.send(&probe).await {
                debug!(error = %e, attempt, "router probe publish failed");
            }
            let wait_until = (Instant::now() + ROUTER_PROBE_INTERVAL).min(deadline);
            while let Ok(Some(env)) = tokio::time::timeout_at(wait_until, inbox.recv()).await {
                if env.meta.id == probe.meta.id {
                    info!(attempt, "router is routing; resuming waves");
                    return;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        warn!(
            "router readiness not confirmed within {}s; resuming anyway",
            ROUTER_READY_TIMEOUT.as_secs()
        );
    }

    pub(super) async fn persist_task(&self, task: &WaveTaskRecord) {
        if let Err(e) = self.storage.create_wave_task(task.clone()).await {
            warn!(error = %e, wave_id = %task.wave_id, task_id = %task.task_id,
                  "failed to persist wave task");
        }
    }

    pub(super) async fn publish_wave_event(&self, wave_id: &str, event_type: &str, data: Value) {
        let channel = subjects::wave_channel_name(wave_id);
        let env = Envelope::new(
            ORCHESTRATOR_IDENTITY,
            channel,
            MessageKind::Event,
            event_payload(event_type, data),
        );
        if let Err(e) = self.client.send(&env).await {
            warn!(error = %e, %wave_id, %event_type, "failed to publish wave event");
        }
    }
}
