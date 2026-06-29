//! High-level NATS client wrapper for nats-hub.
//!
//! Provides connect, publish, subscribe, and a typed envelope API
//! so agents and humans don't have to deal with raw subject strings.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;

use crate::protocol::{subjects, Envelope, MessageKind};

/// Default NATS URL.
pub const DEFAULT_NATS_URL: &str = "nats://127.0.0.1:4222";

/// Hub client — wraps an `async_nats::Client` with typed helpers.
#[derive(Clone)]
pub struct HubClient {
    nats: async_nats::Client,
    /// Identity of whoever is using this client (agent name, human name, etc.).
    identity: String,
}

impl HubClient {
    /// Connect to a NATS server with a given identity.
    pub async fn connect(url: &str, identity: impl Into<String>) -> Result<Self> {
        let identity = identity.into();
        debug!(%url, %identity, "connecting to NATS");
        let nats = async_nats::connect(url)
            .await
            .with_context(|| format!("failed to connect to NATS at {url}"))?;
        Ok(Self { nats, identity })
    }

    /// Return the identity string.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    // ── Typed envelope API ──────────────────────────────────────────

    /// Send an envelope on the bus (publishes to `hub.send.<channel>`).
    /// Flushes after publish to ensure the message reaches the server
    /// before the caller disconnects (critical for short-lived CLI tools).
    pub async fn send(&self, env: &Envelope) -> Result<()> {
        let subject = subjects::send(&env.meta.channel);
        let bytes = env.to_json_bytes()?;
        debug!(%subject, id = %env.meta.id, "publishing envelope");
        self.nats
            .publish(subject.clone(), bytes.into())
            .await
            .with_context(|| format!("publish to {subject} failed"))?;
        self.nats.flush().await.context("flush after publish failed")?;
        Ok(())
    }

    /// Convenience: build + send a message envelope in one call.
    pub async fn send_message(
        &self,
        channel: &str,
        payload: serde_json::Value,
    ) -> Result<String> {
        let env = Envelope::new(self.identity.clone(), channel, MessageKind::Message, payload);
        let id = env.meta.id.clone();
        self.send(&env).await?;
        Ok(id)
    }

    /// Convenience: build + send a status envelope.
    pub async fn send_status(
        &self,
        channel: &str,
        status: impl Into<String>,
    ) -> Result<String> {
        let payload = serde_json::json!({ "status": status.into() });
        let env = Envelope::new(self.identity.clone(), channel, MessageKind::Status, payload);
        let id = env.meta.id.clone();
        self.send(&env).await?;
        Ok(id)
    }

    // ── Subscription API ─────────────────────────────────────────────

    /// Subscribe to a channel subject (e.g. `channel.agents.worker1`).
    ///
    /// Returns a stream of decoded envelopes.
    pub async fn subscribe_channel(
        &self,
        channel: &str,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<Envelope>> {
        let subject = subjects::channel(channel);
        self.subscribe_subject(&subject).await
    }

    /// Subscribe to a raw NATS subject with wildcard support.
    pub async fn subscribe_subject(
        &self,
        subject: &str,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<Envelope>> {
        let subject_owned = subject.to_string();
        let mut sub = self
            .nats
            .subscribe(subject_owned.clone())
            .await
            .with_context(|| format!("subscribe to {subject_owned} failed"))?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(msg) = sub.next().await {
                match Envelope::from_json_bytes(&msg.payload) {
                    Ok(env) => {
                        if tx.send(env).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to decode envelope on {subject_owned}");
                    }
                }
            }
        });
        Ok(rx)
    }

    /// Subscribe to all channels (wildcard `channel.>`).
    pub async fn subscribe_all(&self) -> Result<tokio::sync::mpsc::UnboundedReceiver<Envelope>> {
        self.subscribe_subject("channel.>").await
    }

    // ── Registration ──────────────────────────────────────────────────

    /// Announce this client on the registration subject.
    pub async fn register(&self, capabilities: Vec<String>) -> Result<()> {
        let payload = serde_json::json!({
            "identity": self.identity,
            "capabilities": capabilities,
        });
        let env = Envelope::new(self.identity.clone(), "system", MessageKind::Control, payload);
        let bytes = env.to_json_bytes()?;
        self.nats
            .publish(subjects::REGISTER, bytes.into())
            .await?;
        self.nats.flush().await?;
        Ok(())
    }

    /// Send a heartbeat on the presence subject.
    pub async fn heartbeat(&self) -> Result<()> {
        let payload = serde_json::json!({
            "identity": self.identity,
            "alive": true,
        });
        let env = Envelope::new(self.identity.clone(), "system", MessageKind::Control, payload);
        let bytes = env.to_json_bytes()?;
        self.nats
            .publish(subjects::PRESENCE, bytes.into())
            .await?;
        self.nats.flush().await?;
        Ok(())
    }

    /// Drain and close the connection cleanly.
    pub async fn drain(&self) -> Result<()> {
        self.nats.drain().await?;
        Ok(())
    }
}

/// Track known agents on the bus by listening to registrations + presence.
#[derive(Default)]
pub struct AgentRegistry {
    agents: Arc<Mutex<std::collections::HashMap<String, AgentInfo>>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentInfo {
    pub identity: String,
    pub capabilities: Vec<String>,
    pub last_seen: chrono::DateTime<chrono::Utc>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register(&self, identity: String, capabilities: Vec<String>) {
        self.agents.lock().await.insert(
            identity.clone(),
            AgentInfo {
                identity,
                capabilities,
                last_seen: chrono::Utc::now(),
            },
        );
    }

    pub async fn list(&self) -> Vec<AgentInfo> {
        self.agents.lock().await.values().cloned().collect()
    }
}