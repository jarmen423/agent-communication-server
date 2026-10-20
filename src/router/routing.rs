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

/// Parse a bound send subject `hub.pub.<identity>.<channel>` into
/// `(identity, channel)`. `None` when the subject is not under `hub.pub.`
/// or has no channel token. The identity is the first token only —
/// characters beyond `[A-Za-z0-9_-]` are rejected by the caller via
/// [`crate::protocol::valid_identity`].
pub fn bound_send_subject(subject: &str) -> Option<(&str, &str)> {
    let rest = subject
        .strip_prefix(subjects::PUB_PREFIX)
        .and_then(|r| r.strip_prefix('.'))?;
    let (identity, channel) = rest.split_once('.')?;
    if channel.is_empty() {
        return None;
    }
    Some((identity, channel))
}

/// Parse a bound register/presence subject `hub.<name>.<identity>` into the
/// identity token. `None` when the tail is not exactly one token.
pub fn bound_identity_subject<'a>(subject: &'a str, prefix: &str) -> Option<&'a str> {
    let ident = subject
        .strip_prefix(prefix)
        .and_then(|r| r.strip_prefix('.'))?;
    if ident.is_empty() || ident.contains('.') {
        return None;
    }
    Some(ident)
}

/// Where the router delivers an envelope for a subject-derived `channel`:
///
/// - `meta.to` set → `channel.inbox.<to>` (private DM; the channel is ignored)
/// - `meta.to` unset → `channel.<channel>` (broadcast), where `<channel>`
///   comes from the *subject*, not from `meta.channel`
pub fn route_channel(channel: &str, env: &Envelope) -> String {
    match env.meta.to.as_deref() {
        Some(to) => subjects::inbox(to),
        None => subjects::channel(channel),
    }
}

/// Where the router delivers an envelope that arrived on `send_subject`
/// (`hub.send.<channel>`). Pure function: no I/O, so the routing contract
/// is unit-testable.
pub fn route_subject(send_subject: &str, env: &Envelope) -> String {
    route_channel(channel_from_send_subject(send_subject), env)
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
