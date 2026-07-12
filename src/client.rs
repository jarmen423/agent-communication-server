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
        self.nats
            .flush()
            .await
            .context("flush after publish failed")?;
        Ok(())
    }

    /// Convenience: build + send a message envelope in one call.
    pub async fn send_message(&self, channel: &str, payload: serde_json::Value) -> Result<String> {
        let env = Envelope::new(
            self.identity.clone(),
            channel,
            MessageKind::Message,
            payload,
        );
        let id = env.meta.id.clone();
        self.send(&env).await?;
        Ok(id)
    }

    /// NATS request/reply helper (JSON in, JSON out). Visualizer → worker supervisor.
    pub async fn request_json(
        &self,
        subject: &str,
        payload: serde_json::Value,
        timeout: std::time::Duration,
    ) -> Result<serde_json::Value> {
        let bytes = serde_json::to_vec(&payload).context("serialize request payload")?;
        let resp = tokio::time::timeout(timeout, self.nats.request(subject.to_string(), bytes.into()))
            .await
            .with_context(|| format!("request {subject} timed out"))?
            .with_context(|| format!("request {subject} failed"))?;
        let v: serde_json::Value =
            serde_json::from_slice(&resp.payload).context("decode request reply JSON")?;
        Ok(v)
    }

    /// Convenience: build + send a status envelope.
    pub async fn send_status(&self, channel: &str, status: impl Into<String>) -> Result<String> {
        let payload = serde_json::json!({ "status": status.into() });
        let env = Envelope::new(self.identity.clone(), channel, MessageKind::Status, payload);
        let id = env.meta.id.clone();
        self.send(&env).await?;
        Ok(id)
    }

    /// Send a direct message to a specific agent (DM).
    /// Sets `meta.to` so the router routes to `channel.inbox.<to>`.
    pub async fn send_to(
        &self,
        to: &str,
        channel: &str,
        payload: serde_json::Value,
    ) -> Result<String> {
        let env = Envelope::new(
            self.identity.clone(),
            channel,
            MessageKind::Message,
            payload,
        )
        .to(to);
        let id = env.meta.id.clone();
        self.send(&env).await?;
        Ok(id)
    }

    /// Send a reply to a specific envelope.
    /// Sets `meta.to` (routes to sender's inbox) and `meta.reply_to`
    /// (correlation ID for threading).
    pub async fn send_reply(
        &self,
        original: &Envelope,
        payload: serde_json::Value,
    ) -> Result<String> {
        let env = Envelope::new(
            self.identity.clone(),
            original.meta.channel.clone(),
            MessageKind::Message,
            payload,
        )
        .to(&original.meta.from)
        .reply_to(&original.meta.id);
        let id = env.meta.id.clone();
        self.send(&env).await?;
        Ok(id)
    }

    /// Subscribe to this agent's inbox (DM channel).
    /// Returns a stream of envelopes addressed directly to this identity.
    pub async fn subscribe_inbox(&self) -> Result<tokio::sync::mpsc::UnboundedReceiver<Envelope>> {
        let subject = crate::protocol::subjects::inbox(&self.identity);
        self.subscribe_subject(&subject).await
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

    // ── Session API ────────────────────────────────────────────────

    /// Start a session with a worker. Generates a session UUID, DMs the
    /// worker on their inbox with `action = "session_start"`, and returns
    /// the session UUID.
    ///
    /// The caller should `subscribe_session(uuid)` to receive the
    /// worker's `status: ready` reply and subsequent messages.
    pub async fn start_session(&self, worker: &str, payload: serde_json::Value) -> Result<String> {
        let session_id = uuid::Uuid::new_v4().to_string()[..8].to_string();
        let session_channel = format!("session.{session_id}");

        let mut full_payload = payload.clone();
        if let serde_json::Value::Object(ref mut map) = full_payload {
            map.insert("action".to_string(), serde_json::json!("session_start"));
            map.insert("session_id".to_string(), serde_json::json!(session_id));
            map.insert(
                "session_channel".to_string(),
                serde_json::json!(session_channel),
            );
        } else {
            full_payload = serde_json::json!({
                "action": "session_start",
                "session_id": session_id,
                "session_channel": session_channel,
                "data": payload,
            });
        }

        let env = Envelope::new(
            self.identity.clone(),
            &session_channel,
            MessageKind::Message,
            full_payload,
        )
        .to(worker);
        self.send(&env).await?;
        Ok(session_id)
    }

    /// Send a follow-up message on an existing session channel.
    pub async fn send_to_session(
        &self,
        session_id: &str,
        payload: serde_json::Value,
    ) -> Result<String> {
        let channel = format!("session.{session_id}");
        self.send_message(&channel, payload).await
    }

    /// Close a session (sends `action = "session_close"` on the session channel).
    pub async fn close_session(&self, session_id: &str) -> Result<()> {
        let channel = format!("session.{session_id}");
        let payload = serde_json::json!({"action": "session_close"});
        self.send_message(&channel, payload).await?;
        Ok(())
    }

    /// Subscribe to a session channel to receive events + messages.
    pub async fn subscribe_session(
        &self,
        session_id: &str,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<Envelope>> {
        let channel = format!("session.{session_id}");
        self.subscribe_channel(&channel).await
    }

    // ── Registration ──────────────────────────────────────────────────

    /// Announce this client on the registration subject.
    pub async fn register(&self, capabilities: Vec<String>) -> Result<()> {
        let payload = serde_json::json!({
            "identity": self.identity,
            "capabilities": capabilities,
        });
        let env = Envelope::new(
            self.identity.clone(),
            "system",
            MessageKind::Control,
            payload,
        );
        let bytes = env.to_json_bytes()?;
        self.nats.publish(subjects::REGISTER, bytes.into()).await?;
        self.nats.flush().await?;
        Ok(())
    }

    /// Send a heartbeat on the presence subject.
    pub async fn heartbeat(&self) -> Result<()> {
        let payload = serde_json::json!({
            "identity": self.identity,
            "alive": true,
        });
        let env = Envelope::new(
            self.identity.clone(),
            "system",
            MessageKind::Control,
            payload,
        );
        let bytes = env.to_json_bytes()?;
        self.nats.publish(subjects::PRESENCE, bytes.into()).await?;
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

    /// Update an agent's liveness (called on heartbeat). Preserves capabilities
    /// if the agent was already registered; otherwise seeds with empty caps.
    pub async fn touch(&self, identity: &str) {
        let mut map = self.agents.lock().await;
        let now = chrono::Utc::now();
        match map.get_mut(identity) {
            Some(existing) => existing.last_seen = now,
            None => {
                map.insert(
                    identity.to_string(),
                    AgentInfo {
                        identity: identity.to_string(),
                        capabilities: vec![],
                        last_seen: now,
                    },
                );
            }
        }
    }

    pub async fn list(&self) -> Vec<AgentInfo> {
        self.agents.lock().await.values().cloned().collect()
    }

    /// In-memory: return agents that have ALL the given capabilities.
    pub async fn find_by_capability(&self, caps: &[String]) -> Vec<AgentInfo> {
        if caps.is_empty() {
            return self.list().await;
        }
        self.agents
            .lock()
            .await
            .values()
            .filter(|a| caps.iter().all(|c| a.capabilities.contains(c)))
            .cloned()
            .collect()
    }

    /// In-memory: return agents whose `last_seen` is within the given window.
    pub async fn find_alive(&self, within_secs: i64) -> Vec<AgentInfo> {
        let cutoff = chrono::Utc::now() - chrono::Duration::seconds(within_secs);
        self.agents
            .lock()
            .await
            .values()
            .filter(|a| a.last_seen >= cutoff)
            .cloned()
            .collect()
    }

    /// In-memory: remove an agent. Returns true if it existed.
    pub async fn deregister(&self, identity: &str) -> bool {
        self.agents.lock().await.remove(identity).is_some()
    }

    /// In-memory: force-set an agent's `last_seen` (used when warming the
    /// cache from storage, so we preserve the real DB-recorded liveness
    /// instead of overwriting it with the current time).
    pub async fn force_last_seen(&self, identity: &str, last_seen: chrono::DateTime<chrono::Utc>) {
        let mut map = self.agents.lock().await;
        if let Some(existing) = map.get_mut(identity) {
            existing.last_seen = last_seen;
        }
    }
}
