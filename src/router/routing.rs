//! Pure routing decisions + the capability routing table.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::debug;

use crate::protocol::{subjects, Envelope};

/// Channel name carried by a `hub.send.<channel>` subject, or `"unknown"`
/// if the subject does not have the send prefix.
pub fn channel_from_send_subject(subject: &str) -> &str {
    subject
        .strip_prefix(subjects::SEND_PREFIX)
        .and_then(|rest| rest.strip_prefix('.'))
        .unwrap_or("unknown")
}

/// Where the router delivers an envelope that arrived on `send_subject`
/// (`hub.send.<channel>`):
///
/// - `meta.to` set → `channel.inbox.<to>` (private DM; the channel is ignored)
/// - `meta.to` unset → `channel.<channel>` (broadcast), where `<channel>`
///   comes from the *subject*, not from `meta.channel`
///
/// Pure function: no I/O, so the routing contract is unit-testable.
pub fn route_subject(send_subject: &str, env: &Envelope) -> String {
    match env.meta.to.as_deref() {
        Some(to) => subjects::inbox(to),
        None => subjects::channel(channel_from_send_subject(send_subject)),
    }
}

/// Capability index: capability name → identities that registered it.
///
/// Informational only: routing is decided by [`route_subject`] and never
/// consults this table. It is kept in sync with registrations (no
/// duplicates; re-registering replaces an agent's capabilities) and is
/// exposed for introspection via `ControlPlane::routing_table`.
#[derive(Default, Clone)]
pub struct RoutingTable {
    /// capability/channel name → subscriber identities (insertion order, unique)
    routes: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl RoutingTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a subscriber to a channel's route list. Idempotent.
    pub async fn add_subscriber(&self, channel: &str, identity: &str) {
        let mut routes = self.routes.lock().await;
        let subs = routes.entry(channel.to_string()).or_default();
        if !subs.iter().any(|s| s == identity) {
            subs.push(identity.to_string());
            debug!(%channel, %identity, "routing table: subscriber added");
        }
    }

    /// Remove an identity from every route list (dropping emptied lists).
    pub async fn remove_identity(&self, identity: &str) {
        let mut routes = self.routes.lock().await;
        routes.retain(|_, subs| {
            subs.retain(|s| s != identity);
            !subs.is_empty()
        });
    }

    /// Replace an identity's entries with exactly `capabilities`.
    pub async fn set_capabilities(&self, identity: &str, capabilities: &[String]) {
        let mut routes = self.routes.lock().await;
        routes.retain(|_, subs| {
            subs.retain(|s| s != identity);
            !subs.is_empty()
        });
        for cap in capabilities {
            let subs = routes.entry(cap.clone()).or_default();
            if !subs.iter().any(|s| s == identity) {
                subs.push(identity.to_string());
            }
        }
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
