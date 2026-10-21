//! Holder liveness: the `hub.presence` feed, registry seeding, and the
//! expiry sweep — the same liveness-TTL idea as wave workers.
//!
//! A lock expires when its `expires_at` passes (TTL) **or** when its holder
//! — previously seen on the presence feed — goes silent for longer than
//! `liveness_ttl`. A holder never seen on the feed can't "lose" presence:
//! its locks live until their TTL (or release).

use chrono::Utc;
use serde_json::json;
use std::collections::HashSet;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::protocol::{subjects, Envelope};
use crate::storage::AgentFilter;

use super::LockService;

impl LockService {
    /// Record a heartbeat. On the bound `hub.presence.<identity>` subject
    /// the identity comes from the subject (pinned to the credential by
    /// NATS permissions); only the legacy subject falls back to the
    /// payload, and not at all under `--require-bound-identity`. Same rule
    /// as the wave orchestrator.
    pub(super) async fn on_presence(&self, subject: &str, env: &Envelope) {
        let bound = subject
            .strip_prefix(subjects::PRESENCE)
            .and_then(|rest| rest.strip_prefix('.'))
            .filter(|id| !id.is_empty());
        if bound.is_none() && self.config.require_bound_identity {
            debug!("ignoring legacy hub.presence heartbeat (bound identity required)");
            return;
        }
        let ident = bound.or_else(|| env.payload.get("identity").and_then(|v| v.as_str()));
        if let Some(ident) = ident {
            self.inner
                .lock()
                .await
                .last_seen
                .insert(ident.to_string(), Instant::now());
        }
    }

    /// Seed `last_seen` from the persisted agent registry so a hub-server
    /// restart doesn't instantly expire locks held by agents that were
    /// alive moments ago.
    pub(super) async fn seed_liveness(&self) {
        let within = self.config.liveness_ttl.as_secs().max(1) as i64;
        match self
            .storage
            .find_agents(&AgentFilter::new().alive_within(within))
            .await
        {
            Ok(agents) => {
                let mut inner = self.inner.lock().await;
                for a in agents {
                    let inst = instant_from_datetime(a.last_seen);
                    inner
                        .last_seen
                        .entry(a.identity)
                        .and_modify(|e| {
                            if inst > *e {
                                *e = inst;
                            }
                        })
                        .or_insert(inst);
                }
            }
            Err(e) => warn!(error = %e, "lock service: failed to seed liveness from storage"),
        }
    }

    /// One expiry pass: TTL-dead locks and locks whose holder lost
    /// presence. Each expiry deletes the row and broadcasts
    /// `lock.expired` on the scope's channel.
    pub(super) async fn sweep(&self) {
        let now_utc = Utc::now();
        let now_inst = Instant::now();
        let locks = match self.storage.list_locks(None).await {
            Ok(l) => l,
            Err(e) => {
                warn!(error = %e, "lock service: sweep failed to list locks");
                return;
            }
        };
        // Forget presence entries for identities that hold nothing.
        {
            let holders: HashSet<&str> = locks.iter().map(|l| l.holder.as_str()).collect();
            let mut inner = self.inner.lock().await;
            inner.last_seen.retain(|id, _| holders.contains(id.as_str()));
        }
        let ttl = self.config.liveness_ttl;
        for lock in locks {
            let reason = if lock.expires_at <= now_utc {
                Some("ttl")
            } else {
                let dead = {
                    let inner = self.inner.lock().await;
                    inner
                        .last_seen
                        .get(&lock.holder)
                        .is_some_and(|seen| now_inst.duration_since(*seen) > ttl)
                };
                dead.then_some("presence")
            };
            let Some(reason) = reason else { continue };
            info!(lock_id = %lock.lock_id, holder = %lock.holder, %reason, "lock expired");
            match self.storage.delete_lock(&lock.lock_id).await {
                Ok(true) => {
                    self.publish_event(
                        "lock.expired",
                        json!({"lock_id": lock.lock_id, "scope": lock.scope,
                               "holder": lock.holder, "paths": lock.paths,
                               "reason": reason}),
                    )
                    .await;
                }
                Ok(false) => {}
                Err(e) => warn!(error = %e, lock_id = %lock.lock_id, "lock service: expiry delete failed"),
            }
        }
    }
}

/// Approximate a stored datetime as a monotonic instant (same helper idea
/// as the wave orchestrator's liveness seeding).
fn instant_from_datetime(dt: chrono::DateTime<Utc>) -> Instant {
    let age = (Utc::now() - dt).to_std().unwrap_or_default();
    Instant::now().checked_sub(age).unwrap_or_else(Instant::now)
}
