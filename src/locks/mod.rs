//! File-lock service — the server side of advisory file locks (W2-D).
//!
//! Locks are persisted through [`Storage`] (they survive a hub-server
//! restart). The service owns every mutation (`lock.acquire`, `lock.renew`,
//! `lock.release`): a single `tokio::sync::Mutex` serializes them inside
//! hub-server — the only process allowed to write — so two acquires of
//! overlapping paths can never both win. Reads (`lock.list`, `lock.get`)
//! go straight to storage.
//!
//! The holder identity comes **only** from the caller's bound subject
//! (`hub.api.<identity>.<op>`), never the payload — the same guarantee the
//! router gives `meta.from`. Only the holder renews or releases; an
//! `--api-admin` may release anything (force).
//!
//! Expiry has two triggers:
//!
//! - **TTL** — `expires_at`, renewed by `lock.renew`.
//! - **Presence loss** — the holder is silent on `hub.presence` for longer
//!   than `liveness_ttl` (the same idea as wave worker liveness, fed by the
//!   same presence subjects). A holder never seen on the presence feed
//!   falls back to TTL expiry only.
//!
//! Every acquire/release/expiry is broadcast as a `MessageKind::Event` on
//! `channel.lock.<scope-hash>` so waiters are notified; waiters re-check
//! `lock.list` periodically too, so a missed event can't hang them.
//!
//! The query API reaches the running service through a process-wide handle
//! installed by hub-server, the same pattern as the wave orchestrator.
//! When no service is installed (unit tests, a hub without storage) the
//! write ops return a clear "not active" error; the read ops still work —
//! they only need storage.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::time::Instant;
use tracing::warn;

use crate::client::HubClient;
use crate::protocol::subjects;
use crate::query_api::{ApiAuthz, ApiResponse, Caller};
use crate::storage::Storage;

mod liveness;
mod ops;

pub use nats_hub_client::locks::{
    lock_channel, wait_for_locks, AcquireOptions, AcquireOutcome, LockList, WaitOutcome,
    LOCKS_IDENTITY,
};

/// How long a lock holder may be silent on `hub.presence` before its locks
/// count as lost. Mirrors the wave worker-liveness default (heartbeats run
/// every ~30 s).
pub const DEFAULT_LOCK_LIVENESS_SECS: u64 = 90;

/// Configuration for the lock service.
#[derive(Debug, Clone)]
pub struct LockConfig {
    /// Presence-loss TTL for lock holders.
    pub liveness_ttl: Duration,
    /// Drop legacy `hub.presence` payload identities
    /// (`--require-bound-identity`): only the bound subject token counts.
    pub require_bound_identity: bool,
}

impl Default for LockConfig {
    fn default() -> Self {
        Self {
            liveness_ttl: Duration::from_secs(DEFAULT_LOCK_LIVENESS_SECS),
            require_bound_identity: false,
        }
    }
}

/// In-memory state that never leaves hub-server: the conflict-check mutex
/// and the holder-presence map.
#[derive(Default)]
struct Inner {
    /// identity → most recent proof of life on `hub.presence` (seeded from
    /// the persisted agent registry on start).
    last_seen: HashMap<String, Instant>,
}

/// The running lock service. `client` is `None` in unit tests — ops still
/// work; only event publishing and the presence sweep are skipped.
pub struct LockService {
    pub(crate) storage: Arc<dyn Storage>,
    client: Option<HubClient>,
    config: LockConfig,
    inner: tokio::sync::Mutex<Inner>,
}

impl LockService {
    /// A service over storage only (no NATS): ops work, nothing publishes
    /// events and presence is never tracked. For tests.
    pub fn new(storage: Arc<dyn Storage>) -> Arc<Self> {
        Self::with_config(storage, LockConfig::default())
    }

    /// [`new`] with an explicit config.
    pub fn with_config(storage: Arc<dyn Storage>, config: LockConfig) -> Arc<Self> {
        Arc::new(Self {
            storage,
            client: None,
            config,
            inner: tokio::sync::Mutex::new(Inner::default()),
        })
    }

    /// Connect to NATS as `hub-locks`, install the process-wide handle and
    /// spawn the presence/sweep loop. On a connect failure a storage-only
    /// service is installed instead — lock ops still work (degraded: no
    /// events, no presence expiry), and the caller logs the cause.
    pub async fn start(
        storage: Arc<dyn Storage>,
        nats_url: &str,
        config: LockConfig,
    ) -> Result<Arc<Self>> {
        let svc = match HubClient::connect(nats_url, LOCKS_IDENTITY).await {
            Ok(client) => Arc::new(Self {
                storage,
                client: Some(client),
                config,
                inner: tokio::sync::Mutex::new(Inner::default()),
            }),
            Err(e) => {
                warn!(error = %e, "lock service: NATS connect failed — locks work without events/presence expiry");
                Self::with_config(storage, config)
            }
        };
        install(svc.clone());
        if svc.client.is_some() {
            let runner = svc.clone();
            tokio::spawn(async move {
                if let Err(e) = runner.run().await {
                    warn!(error = %e, "lock service exited");
                }
            });
        }
        Ok(svc)
    }

    /// The presence + expiry loop: heartbeats on `hub.presence` /
    /// `hub.presence.<id>` keep holders alive; the periodic sweep expires
    /// TTL-dead and presence-lost locks.
    async fn run(self: &Arc<Self>) -> Result<()> {
        let client = self.client.as_ref().context("lock service: no client")?;
        let mut legacy_rx = client
            .subscribe_subject_tagged(subjects::PRESENCE)
            .await
            .context("lock service: subscribe hub.presence failed")?;
        let mut bound_rx = client
            .subscribe_subject_tagged(&format!("{}.*", subjects::PRESENCE))
            .await
            .context("lock service: subscribe hub.presence.* failed")?;

        self.seed_liveness().await;

        // Often enough to catch a dead holder near its TTL.
        let period = (self.config.liveness_ttl / 3)
            .clamp(Duration::from_secs(1), Duration::from_secs(10));
        let mut sweep = tokio::time::interval(period);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                Some((subj, env)) = legacy_rx.recv() => self.on_presence(&subj, &env).await,
                Some((subj, env)) = bound_rx.recv() => self.on_presence(&subj, &env).await,
                _ = sweep.tick() => self.sweep().await,
            }
        }
    }

    /// Publish a `lock.<event>` envelope on the scope's lock channel.
    /// Fire-and-forget: a failed publish is logged, never fatal.
    pub(crate) async fn publish_event(&self, event_type: &str, data: Value) {
        let Some(client) = &self.client else {
            return;
        };
        let scope = data.get("scope").and_then(Value::as_str).unwrap_or("");
        let channel = nats_hub_client::locks::lock_channel(scope);
        let env = crate::Envelope::new(
            LOCKS_IDENTITY,
            channel,
            crate::MessageKind::Event,
            crate::events::event_payload(event_type, data),
        );
        if let Err(e) = client.send(&env).await {
            warn!(error = %e, %event_type, "lock service: event publish failed");
        }
    }
}

// ── Process-wide handle (same pattern as the wave orchestrator) ──────────

static SERVICE: RwLock<Option<Arc<LockService>>> = RwLock::new(None);

/// Install the service the query API dispatches `lock.*` writes to. Called
/// by hub-server at startup (replaces any previous install — tests swap
/// services this way).
pub fn install(svc: Arc<LockService>) {
    *SERVICE.write().unwrap_or_else(|e| e.into_inner()) = Some(svc);
}

/// The installed service, when one runs in this process.
pub fn service() -> Option<Arc<LockService>> {
    SERVICE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Remove the installed service (test teardown).
#[doc(hidden)]
pub fn uninstall() {
    *SERVICE.write().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Query-API dispatch for `lock.*` ops. `caller` is the subject-bound
/// caller (or legacy); `authz` knows the hub's `--api-admin` list.
pub async fn dispatch_op(
    storage: &dyn Storage,
    caller: &Caller,
    authz: &ApiAuthz,
    op: &str,
    params: &Value,
) -> ApiResponse {
    let admin = match caller {
        Caller::Bound(id) => authz.is_admin(id),
        Caller::Legacy => false,
    };
    match op {
        "lock.list" => ops::list(storage, params).await,
        "lock.get" => ops::get(storage, params).await,
        "lock.acquire" | "lock.renew" | "lock.release" => match service() {
            Some(svc) => svc.handle_write(caller, admin, op, params).await,
            None => ApiResponse::err(format!(
                "{op}: file locks require a running hub-server with storage \
                 (the lock service is not active)"
            )),
        },
        _ => ApiResponse::err(format!("unknown operation: {op}")),
    }
}

/// Identity the caller locks as. Bound callers always hold under their own
/// subject identity — a `holder` param may only repeat it. A legacy caller
/// (reachable only on a permissive hub) must name `holder` itself —
/// self-asserted, exactly like `meta.from` on a permissive hub.
fn resolve_holder(caller: &Caller, params: &Value) -> Result<String, ApiResponse> {
    let claimed = params.get("holder").and_then(Value::as_str);
    match caller {
        Caller::Bound(id) => match claimed {
            Some(h) if h != id => Err(ApiResponse::err(format!(
                "holder '{h}' does not match the bound caller '{id}' — the lock \
                 holder is always your subject identity, never a payload field"
            ))),
            _ => Ok(id.clone()),
        },
        Caller::Legacy => match claimed {
            Some(h) if crate::protocol::valid_identity(h) => Ok(h.to_string()),
            Some(h) => Err(ApiResponse::err(format!("invalid holder '{h}'"))),
            None => Err(ApiResponse::err(
                "missing holder — legacy hub.api.<op> callers must pass one; \
                 bound callers get their subject identity",
            )),
        },
    }
}

/// May `caller` mutate `lock`? The holder may; an `--api-admin` may
/// (force). A legacy caller on a permissive hub must assert the holder.
fn check_holder(
    caller: &Caller,
    admin: bool,
    params: &Value,
    holder: &str,
) -> Result<(), ApiResponse> {
    if admin {
        return Ok(());
    }
    match resolve_holder(caller, params) {
        Ok(id) if id == holder => Ok(()),
        Ok(id) => Err(ApiResponse::err(format!(
            "forbidden: lock is held by '{holder}', not '{id}' — only the holder \
             may renew or release it (admins can force-release)"
        ))),
        Err(resp) => Err(resp),
    }
}

/// Debug helper for tests: record one presence proof without NATS.
#[doc(hidden)]
pub async fn note_presence(svc: &LockService, subject: &str, env: &crate::Envelope) {
    svc.on_presence(subject, env).await;
}

/// Debug helper for tests: run one expiry sweep synchronously.
#[doc(hidden)]
pub async fn sweep_once(svc: &LockService) {
    svc.sweep().await;
}
