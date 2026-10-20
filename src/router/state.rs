//! Router state and per-message routing/registration logic, independent of
//! the NATS connection so it can be exercised without a server.

use std::sync::{Arc, OnceLock};

use tracing::{debug, info, warn};

use super::mirror::{MirrorOp, StorageMirror};
use super::routing::{
    bound_identity_subject, bound_send_subject, channel_from_send_subject, route_channel,
    RoutingTable,
};
use crate::client::AgentRegistry;
use crate::protocol::{subjects, valid_identity, Envelope};
use crate::storage::AgentRecord;
use crate::MetricsCollector;

/// Router state and per-message logic, independent of the NATS connection
/// (so it can be exercised without a server).
pub(crate) struct RouterCore {
    pub(super) routing: RoutingTable,
    pub(super) registry: AgentRegistry,
    pub(super) metrics: Option<Arc<MetricsCollector>>,
    /// When true, legacy self-asserted subjects (`hub.send.*`, `hub.register`,
    /// `hub.presence`) are dropped instead of routed; only bound subjects
    /// carrying the connection's identity are accepted.
    pub(super) require_bound: bool,
    /// Optional WS bridge broadcast channel. When set, every routed envelope
    /// is serialized to JSON and pushed to all connected visualizer clients.
    pub(super) ws_event_tx: Option<crate::ws_bridge::EventTx>,
    /// Set once by `ControlPlane::run` when storage is attached.
    pub(super) mirror: OnceLock<StorageMirror>,
}

impl RouterCore {
    pub(crate) fn new() -> Self {
        Self {
            routing: RoutingTable::new(),
            registry: AgentRegistry::new(),
            metrics: None,
            require_bound: false,
            ws_event_tx: None,
            mirror: OnceLock::new(),
        }
    }

    /// A message arrived on a legacy, self-asserted subject. Either count it
    /// and let the caller continue (compat mode) or count it and tell the
    /// caller to drop it (`--require-bound-identity`).
    fn legacy_subject(&self, subject: &str) -> bool {
        if let Some(metrics) = &self.metrics {
            metrics.record_unbound();
        }
        if self.require_bound {
            warn!(%subject, "dropping legacy unbound subject (require-bound-identity)");
            return false;
        }
        true
    }

    /// Identity bound to `subject` when it is a bound register/presence
    /// subject, handling validation + legacy-policy bookkeeping. `None`
    /// means the caller must drop the message.
    fn bound_or_legacy_identity(&self, subject: &str, prefix: &str) -> BoundIdentity {
        match bound_identity_subject(subject, prefix) {
            Some(ident) if valid_identity(ident) => BoundIdentity::Bound(ident.to_string()),
            Some(bad) => {
                warn!(%subject, identity = %bad, "invalid identity in bound subject, dropping");
                BoundIdentity::Drop
            }
            None => {
                if self.legacy_subject(subject) {
                    BoundIdentity::Legacy
                } else {
                    BoundIdentity::Drop
                }
            }
        }
    }

    fn enqueue(&self, op: MirrorOp) {
        if let Some(mirror) = self.mirror.get() {
            mirror.enqueue(op);
        }
    }

    /// Decode an envelope from `hub.send.<channel>` (legacy) or
    /// `hub.pub.<identity>.<channel>` (bound), record metrics, push it
    /// to the WS bridge and decide its destination.
    ///
    /// Returns `(dest_subject, envelope, publish_bytes)`. On a bound subject
    /// `meta.from` and `meta.channel` are overwritten from the subject (the
    /// subject is the proof; the payload is self-asserted). On any subject:
    /// an invalid `meta.to` drops the message, and a send on channel
    /// `inbox.<id>` without `meta.to` gets `meta.to = <id>` so storage and
    /// read scoping treat it as the DM it is. `publish_bytes` is the
    /// re-serialized envelope when anything changed, else the original
    /// payload verbatim. `None` = drop (undecodable, invalid identity or
    /// recipient, or a legacy subject while `--require-bound-identity` is on).
    pub(crate) fn on_send(
        &self,
        subject: &str,
        payload: &[u8],
    ) -> Option<(String, Envelope, Vec<u8>)> {
        let mut env = match Envelope::from_json_bytes(payload) {
            Ok(env) => env,
            Err(e) => {
                warn!(%subject, error = %e, "failed to decode envelope, dropping");
                return None;
            }
        };

        let mut changed = false;
        let channel: &str = match bound_send_subject(subject) {
            Some((ident, channel)) if valid_identity(ident) => {
                // The subject is the identity proof: overwrite the
                // self-asserted `meta.from` (and `meta.channel`, which
                // storage and consumers read) before routing/mirroring.
                if env.meta.from != ident || env.meta.channel != channel {
                    env.meta.from = ident.to_string();
                    env.meta.channel = channel.to_string();
                    changed = true;
                }
                channel
            }
            Some((bad, _)) => {
                warn!(%subject, identity = %bad, "invalid identity in bound subject, dropping");
                return None;
            }
            None => {
                if !self.legacy_subject(subject) {
                    return None;
                }
                channel_from_send_subject(subject)
            }
        };

        match env.meta.to.as_deref() {
            Some(to) if !valid_identity(to) => {
                warn!(%subject, to = %to, id = %env.meta.id, "invalid meta.to recipient, dropping");
                return None;
            }
            Some(_) => {}
            None => {
                if let Some(owner) = inbox_channel_owner(channel) {
                    env.meta.to = Some(owner.to_string());
                    changed = true;
                }
            }
        }

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

        let dest = route_channel(channel, &env);
        let publish = if changed {
            match env.to_json_bytes() {
                Ok(bytes) => bytes,
                Err(e) => {
                    warn!(%subject, error = %e, "failed to re-encode envelope, dropping");
                    return None;
                }
            }
        } else {
            payload.to_vec()
        };
        debug!(
            id = %env.meta.id,
            from = %env.meta.from,
            to = ?env.meta.to,
            kind = ?env.meta.kind,
            %dest,
            "routing envelope"
        );
        Some((dest, env, publish))
    }

    /// Queue a routed envelope for the storage mirror (non-blocking).
    pub(crate) fn mirror_envelope(&self, env: Envelope) {
        self.enqueue(MirrorOp::Envelope(Box::new(env)));
    }

    /// `hub.register` / `hub.register.<identity>`: record identity +
    /// capabilities (replacing any previous capabilities) and persist the
    /// registration. On the bound subject the identity comes from the
    /// subject token, not from `payload.identity`.
    pub(crate) async fn on_register(&self, subject: &str, payload: &[u8]) {
        let env = match Envelope::from_json_bytes(payload) {
            Ok(env) => env,
            Err(e) => {
                warn!(error = %e, "failed to decode registration envelope");
                return;
            }
        };
        let bound = match self.bound_or_legacy_identity(subject, subjects::REGISTER) {
            BoundIdentity::Bound(id) => Some(id),
            BoundIdentity::Legacy => None,
            BoundIdentity::Drop => return,
        };
        let Some(ident) = claimed_identity(bound.as_deref(), &env) else {
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

    /// `hub.presence` / `hub.presence.<identity>`: refresh liveness only.
    /// Capabilities are preserved (an unknown agent is added with none
    /// until it registers). On the bound subject the identity comes from
    /// the subject token.
    pub(crate) async fn on_presence(&self, subject: &str, payload: &[u8]) {
        let env = match Envelope::from_json_bytes(payload) {
            Ok(env) => env,
            Err(e) => {
                warn!(error = %e, "failed to decode presence envelope");
                return;
            }
        };
        let bound = match self.bound_or_legacy_identity(subject, subjects::PRESENCE) {
            BoundIdentity::Bound(id) => Some(id),
            BoundIdentity::Legacy => None,
            BoundIdentity::Drop => return,
        };
        let Some(ident) = claimed_identity(bound.as_deref(), &env) else {
            return;
        };
        debug!(identity = %ident, "heartbeat received");
        self.registry.touch(ident).await;
        self.enqueue(MirrorOp::Touch(ident.to_string()));
    }
}

/// Identity a register/presence message applies to: the bound subject's
/// token, else (legacy) `payload.identity` when it is a valid identity.
fn claimed_identity<'a>(bound: Option<&'a str>, env: &'a Envelope) -> Option<&'a str> {
    if bound.is_some() {
        return bound;
    }
    let claimed = env.payload.get("identity").and_then(|v| v.as_str())?;
    if valid_identity(claimed) {
        Some(claimed)
    } else {
        warn!(identity = %claimed, "invalid identity in legacy register/presence payload, dropping");
        None
    }
}

/// `<id>` when `channel` is exactly `inbox.<id>` with a valid identity.
fn inbox_channel_owner(channel: &str) -> Option<&str> {
    channel
        .strip_prefix("inbox.")
        .filter(|id| valid_identity(id))
}

/// How a register/presence subject carries the caller's identity.
enum BoundIdentity {
    /// Bound subject with a valid identity token.
    Bound(String),
    /// Legacy subject, allowed while `--require-bound-identity` is off.
    Legacy,
    /// Drop the message.
    Drop,
}
