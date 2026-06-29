//! Control plane router — the brains of nats-hub.
//!
//! Subscribes to `hub.send.>` (everything agents publish) and routes
//! envelopes to the correct `channel.<name>` subjects. Also listens
//! to `hub.register` to track known agents and maintains a routing
//! table that can be extended with custom rules.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::client::{AgentInfo, AgentRegistry};
use crate::protocol::{subjects, Envelope};

/// Routing rules — maps channel names to endpoint lists.
/// In the base implementation routing is direct (channel → channel.<name>),
/// but this table allows future extension (fan-out, filtering, transforms).
#[derive(Default)]
pub struct RoutingTable {
    /// channel name → list of subscriber identities
    routes: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl RoutingTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a subscriber to a channel's route list.
    pub async fn add_subscriber(&self, channel: &str, identity: &str) {
        self.routes
            .lock()
            .await
            .entry(channel.to_string())
            .or_default()
            .push(identity.to_string());
        debug!(%channel, %identity, "routing table: subscriber added");
    }

    /// Get the list of subscribers for a channel (if any registered).
    pub async fn subscribers(&self, channel: &str) -> Vec<String> {
        self.routes
            .lock()
            .await
            .get(channel)
            .cloned()
            .unwrap_or_default()
    }
}

/// The control plane worker. Runs an event loop that:
/// 1. Subscribes to `hub.send.>` — all outgoing agent messages.
/// 2. Routes each envelope to `channel.<channel>`.
/// 3. Subscribes to `hub.register` — tracks agent registrations.
/// 4. Optionally subscribes to `hub.presence` — tracks heartbeats.
pub struct ControlPlane {
    nats: async_nats::Client,
    routing: RoutingTable,
    registry: AgentRegistry,
}

impl ControlPlane {
    /// Create a new control plane connected to the given NATS URL.
    pub async fn connect(url: &str) -> Result<Self> {
        let nats = async_nats::connect(url)
            .await
            .with_context(|| format!("control plane: connect to {url} failed"))?;
        Ok(Self {
            nats,
            routing: RoutingTable::new(),
            registry: AgentRegistry::new(),
        })
    }

    /// Run the control plane event loop. Blocks until cancelled.
    pub async fn run(&self) -> Result<()> {
        info!("control plane router starting");

        // 1. Subscribe to all agent sends
        let mut send_sub = self
            .nats
            .subscribe("hub.send.>")
            .await
            .context("control plane: subscribe to hub.send.> failed")?;

        // 2. Subscribe to registrations
        let mut reg_sub = self
            .nats
            .subscribe(subjects::REGISTER)
            .await
            .context("control plane: subscribe to hub.register failed")?;

        // 3. Subscribe to presence
        let mut presence_sub = self
            .nats
            .subscribe(subjects::PRESENCE)
            .await
            .context("control plane: subscribe to hub.presence failed")?;

        info!("control plane subscribed to hub.send.>, hub.register, hub.presence");

        loop {
            tokio::select! {
                Some(msg) = send_sub.next() => {
                    self.handle_send(&msg).await;
                }
                Some(msg) = reg_sub.next() => {
                    self.handle_register(&msg).await;
                }
                Some(msg) = presence_sub.next() => {
                    self.handle_presence(&msg).await;
                }
            }
        }
    }

    /// Handle a message on `hub.send.<channel>` — route it to `channel.<channel>`.
    async fn handle_send(&self, msg: &async_nats::Message) {
        // Extract channel from subject: hub.send.<channel>
        let subject = msg.subject.as_str();
        let channel = subject
            .strip_prefix(&format!("{}.", subjects::SEND_PREFIX))
            .unwrap_or("unknown");

        match Envelope::from_json_bytes(&msg.payload) {
            Ok(env) => {
                debug!(
                    id = %env.meta.id,
                    %channel,
                    from = %env.meta.from,
                    kind = ?env.meta.kind,
                    "routing envelope"
                );

                // Publish to channel.<channel> for subscribers
                let dest = subjects::channel(channel);
                if let Err(e) = self
                    .nats
                    .publish(dest.clone(), msg.payload.clone().into())
                    .await
                {
                    error!(%dest, error = %e, "failed to route envelope");
                } else {
                    debug!(%dest, "envelope routed");
                }

                // Flush to ensure routed messages reach subscribers promptly
                let _ = self.nats.flush().await;
            }
            Err(e) => {
                warn!(%subject, error = %e, "failed to decode envelope, dropping");
            }
        }
    }

    /// Handle a registration message.
    async fn handle_register(&self, msg: &async_nats::Message) {
        match Envelope::from_json_bytes(&msg.payload) {
            Ok(env) => {
                if let Some(ident) = env.payload.get("identity").and_then(|v| v.as_str()) {
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
                    // Also add to routing table for channels based on capabilities
                    for cap in &caps {
                        self.routing.add_subscriber(cap, ident).await;
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to decode registration envelope");
            }
        }
    }

    /// Handle a presence heartbeat.
    async fn handle_presence(&self, msg: &async_nats::Message) {
        match Envelope::from_json_bytes(&msg.payload) {
            Ok(env) => {
                if let Some(ident) = env.payload.get("identity").and_then(|v| v.as_str()) {
                    debug!(identity = %ident, "heartbeat received");
                    self.registry.register(ident.to_string(), vec![]).await;
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to decode presence envelope");
            }
        }
    }

    /// Get a snapshot of known agents.
    pub async fn known_agents(&self) -> Vec<AgentInfo> {
        self.registry.list().await
    }
}