//! Advisory file locks (W2-D) — wire helpers and client calls.
//!
//! An agent announces "I'm working on these paths" inside a `scope` (a repo
//! or project key, a free string). Others can list who holds what and wait
//! until the paths are free. This is coordination, not filesystem locking.
//!
//! Server ops live on the query API (`lock.acquire`, `lock.renew`,
//! `lock.release`, `lock.list`, `lock.get`); every acquire, release and
//! expiry is broadcast as a `MessageKind::Event` on
//! `channel.lock.<scope-hash>` (`{event_type: lock.acquired|lock.released|
//! lock.expired, data: {…}}`). The scope hash keeps arbitrary scope strings
//! out of subject tokens; [`lock_channel`] computes it, and every lock API
//! response also carries the channel name so non-Rust clients never need
//! the hash.
//!
//! See `docs/FILE_LOCKS.md`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;

use crate::client::HubClient;
use crate::protocol::MessageKind;
use crate::storage::LockRecord;
use crate::ApiClient;

/// Default lock TTL (15 min).
pub const DEFAULT_TTL_SECS: u64 = 900;
/// Longest lock TTL (24 h).
pub const MAX_TTL_SECS: u64 = 86_400;
/// Shortest lock TTL.
pub const MIN_TTL_SECS: u64 = 1;
/// Identity stamped on lock events the hub publishes.
pub const LOCKS_IDENTITY: &str = "hub-locks";

/// Clamp a requested TTL into `MIN_TTL_SECS..=MAX_TTL_SECS`.
pub fn clamp_ttl(secs: u64) -> u64 {
    secs.clamp(MIN_TTL_SECS, MAX_TTL_SECS)
}

/// Normalize one path entry: trim, drop leading `./` and `/`. `None` when
/// nothing meaningful remains. A trailing `/` marks a directory prefix.
pub fn normalize_path(raw: &str) -> Option<String> {
    let mut p = raw.trim();
    while let Some(rest) = p.strip_prefix("./") {
        p = rest;
    }
    let p = p.trim_start_matches('/');
    (!p.is_empty()).then(|| p.to_string())
}

/// Do two lock paths overlap? Equal paths overlap; a directory prefix
/// (`dir/`) overlaps everything under it and the bare directory name.
pub fn paths_overlap(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
        || (a.ends_with('/') && b.starts_with(a))
        || (b.ends_with('/') && a.starts_with(b))
}

/// Do a lock's paths conflict with `paths` inside the same `scope`?
pub fn paths_conflict(scope: &str, paths: &[String], other: &LockRecord) -> bool {
    other.scope == scope
        && paths
            .iter()
            .any(|p| other.paths.iter().any(|o| paths_overlap(p, o)))
}

/// FNV-1a 64-bit — a tiny, stable, dependency-free hash. Rendered as 16 hex
/// chars; deterministic across Rust and Python implementations.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The `<hash>` token identifying `scope` in `channel.lock.<hash>` subjects.
pub fn scope_hash(scope: &str) -> String {
    format!("{:016x}", fnv1a64(scope.as_bytes()))
}

/// Channel name for a scope's lock events: `lock.<hash>` (no `channel.`
/// prefix — matches `subjects::*_channel_name` conventions). Subscribe on
/// `channel.lock.<hash>` for the routed subject.
pub fn lock_channel(scope: &str) -> String {
    format!("lock.{}", scope_hash(scope))
}

/// Extra knobs accepted by `lock.acquire`.
#[derive(Debug, Clone, Default)]
pub struct AcquireOptions {
    /// Seconds until the lock expires; clamped to `1..=86400`, default 900.
    pub ttl_secs: Option<u64>,
    pub reason: Option<String>,
    pub task_id: Option<String>,
    /// Legacy-subject holder (self-asserted, permissive hubs only). On a
    /// bound call the holder always comes from the subject; this must match
    /// the caller identity or the request is rejected.
    pub holder: Option<String>,
}

/// Outcome of `lock.acquire`: granted (`lock` set) or refused with the
/// conflicting locks.
#[derive(Debug)]
pub struct AcquireOutcome {
    pub granted: bool,
    pub lock: Option<LockRecord>,
    pub conflicts: Vec<LockRecord>,
    /// `lock.<hash>` event channel for this scope.
    pub channel: String,
}

/// Result of [`ApiClient::lock_list`].
#[derive(Debug)]
pub struct LockList {
    /// Live (unexpired) locks. Empty when none.
    pub locks: Vec<LockRecord>,
    /// `lock.<hash>` event channel when a scope was given.
    pub channel: Option<String>,
}

/// How [`wait_for_locks`] ended.
#[derive(Debug)]
pub enum WaitOutcome {
    /// No other holder conflicts with `paths` — proceed.
    Free,
    /// Still blocked at the deadline; carries the conflicting locks.
    TimedOut(Vec<LockRecord>),
}

fn lock_from(v: &Value) -> Result<LockRecord> {
    serde_json::from_value(v.clone()).context("bad lock record in API response")
}

fn locks_from(v: Option<&Value>) -> Vec<LockRecord> {
    v.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    lock_from(r)
                        .map_err(|e| tracing::warn!(%e, "bad lock row"))
                        .ok()
                })
                .collect()
        })
        .unwrap_or_default()
}

impl ApiClient {
    /// `lock.acquire {scope, paths, ttl_secs?, reason?, task_id?}` —
    /// all-or-nothing; on conflict `granted` is false and `conflicts` holds
    /// the blocking locks (never a partial grant). The holder is the bound
    /// caller identity.
    pub async fn lock_acquire(
        &self,
        scope: &str,
        paths: &[String],
        opts: AcquireOptions,
    ) -> Result<AcquireOutcome> {
        let mut params = json!({"scope": scope, "paths": paths});
        if let Some(t) = opts.ttl_secs {
            params["ttl_secs"] = json!(t);
        }
        if let Some(r) = &opts.reason {
            params["reason"] = json!(r);
        }
        if let Some(t) = &opts.task_id {
            params["task_id"] = json!(t);
        }
        if let Some(h) = &opts.holder {
            params["holder"] = json!(h);
        }
        let data = self.request("lock.acquire", params).await?;
        let granted = data
            .get("granted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let lock = data.get("lock").map(lock_from).transpose()?;
        Ok(AcquireOutcome {
            granted,
            lock,
            conflicts: locks_from(data.get("conflicts")),
            channel: data
                .get("channel")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// `lock.renew {lock_id, ttl_secs?}` — extend a lock's expiry. Only the
    /// holder (or an admin) may renew.
    pub async fn lock_renew(&self, lock_id: &str, ttl_secs: Option<u64>) -> Result<LockRecord> {
        let mut params = json!({"lock_id": lock_id});
        if let Some(t) = ttl_secs {
            params["ttl_secs"] = json!(t);
        }
        let data = self.request("lock.renew", params).await?;
        data.get("lock")
            .map(lock_from)
            .transpose()?
            .context("lock.renew: no lock in response")
    }

    /// `lock.release {lock_id}` — release your own lock; admins may also
    /// pass `force` to release anyone's.
    pub async fn lock_release(&self, lock_id: &str, force: bool) -> Result<()> {
        let mut params = json!({"lock_id": lock_id});
        if force {
            params["force"] = json!(true);
        }
        self.request("lock.release", params).await?;
        Ok(())
    }

    /// `lock.list {scope?}` — live locks, optionally scoped. `None` lists
    /// every live lock (bounded by the server limit).
    pub async fn lock_list(&self, scope: Option<&str>) -> Result<LockList> {
        let params = match scope {
            Some(s) => json!({"scope": s}),
            None => json!({}),
        };
        let data = self.request("lock.list", params).await?;
        Ok(LockList {
            locks: locks_from(data.get("locks")),
            channel: data
                .get("channel")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    /// `lock.get {lock_id}` — `None` when the lock is gone or expired.
    pub async fn lock_get(&self, lock_id: &str) -> Result<Option<LockRecord>> {
        let data = self
            .request("lock.get", json!({"lock_id": lock_id}))
            .await?;
        match data.get("lock") {
            Some(v) if !v.is_null() => Ok(Some(lock_from(v)?)),
            _ => Ok(None),
        }
    }
}

/// Locks in `locks` that conflict with `paths` in `scope`, ignoring the
/// caller's own locks (waiting on yourself is a deadlock by definition).
fn blocking_locks(
    my_identity: &str,
    scope: &str,
    paths: &[String],
    locks: &[LockRecord],
) -> Vec<LockRecord> {
    locks
        .iter()
        .filter(|l| l.holder != my_identity && paths_conflict(scope, paths, l))
        .cloned()
        .collect()
}

/// Block until no *other* holder locks `paths` in `scope`, or `timeout`
/// passes. Driven by `channel.lock.<hash>` events, with a re-check every
/// `poll` so a missed event can never hang the waiter.
pub async fn wait_for_locks(
    hub: &HubClient,
    api: &ApiClient,
    scope: &str,
    paths: &[String],
    timeout: Duration,
    poll: Duration,
) -> Result<WaitOutcome> {
    let me = hub.identity().to_string();
    let mut events = hub.subscribe_channel(&lock_channel(scope)).await?;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut pending = blocking_locks(&me, scope, paths, &api.lock_list(Some(scope)).await?.locks);
    while !pending.is_empty() {
        // Wait for the next lock event or the periodic re-check — whichever
        // first; the poll alone guarantees progress without events.
        let _ = tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + poll),
            events.recv(),
        )
        .await;
        if tokio::time::Instant::now() >= deadline {
            return Ok(WaitOutcome::TimedOut(pending));
        }
        pending = blocking_locks(&me, scope, paths, &api.lock_list(Some(scope)).await?.locks);
    }
    Ok(WaitOutcome::Free)
}

/// Decode a lock event envelope: `Some((event_type, lock_id))` for
/// `lock.*` events, `None` for anything else.
pub fn lock_event(env: &crate::Envelope) -> Option<(&str, &serde_json::Value)> {
    if env.meta.kind != MessageKind::Event {
        return None;
    }
    let ty = env.payload.get("event_type")?.as_str()?;
    ty.starts_with("lock.")
        .then(|| (ty, env.payload.get("data").unwrap_or(&env.payload)))
}

/// A lock record as the event `data.lock` field (when present).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockEventData {
    pub lock_id: String,
    pub scope: String,
    pub holder: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_normalization() {
        assert_eq!(normalize_path(" src/lib.rs ").unwrap(), "src/lib.rs");
        assert_eq!(normalize_path("./src/").unwrap(), "src/");
        assert_eq!(normalize_path("/abs/x").unwrap(), "abs/x");
        assert!(normalize_path("  ").is_none());
        assert!(normalize_path("./").is_none());
    }

    #[test]
    fn overlap_rules() {
        assert!(paths_overlap("a/b.rs", "a/b.rs"));
        assert!(paths_overlap("a/", "a/b.rs"));
        assert!(paths_overlap("a/b.rs", "a/"));
        assert!(paths_overlap("a/", "a/"));
        assert!(paths_overlap("a", "a/")); // bare name vs dir prefix — same node
        assert!(!paths_overlap("a/b.rs", "a/c.rs"));
        assert!(!paths_overlap("ab.rs", "a/"));
        assert!(!paths_overlap("a", "ab")); // "ab" is not under dir "a/"
    }

    #[test]
    fn conflict_needs_same_scope_and_overlap() {
        let lock = LockRecord {
            lock_id: "l1".into(),
            scope: "repo".into(),
            paths: vec!["src/".into()],
            holder: "a".into(),
            reason: None,
            task_id: None,
            ttl_secs: 60,
            created_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now(),
        };
        assert!(paths_conflict("repo", &["src/x.rs".into()], &lock));
        assert!(!paths_conflict("repo", &["other.rs".into()], &lock));
        assert!(!paths_conflict("other-scope", &["src/x.rs".into()], &lock));
    }

    #[test]
    fn fnv1a_vectors() {
        // FNV-1a 64-bit reference values (same algorithm Python mirrors).
        assert_eq!(scope_hash(""), "cbf29ce484222325");
        assert_eq!(scope_hash("repo"), "4cac581fbfac663d");
        assert_eq!(lock_channel("repo"), "lock.4cac581fbfac663d");
    }

    #[test]
    fn ttl_clamps() {
        assert_eq!(clamp_ttl(0), MIN_TTL_SECS);
        assert_eq!(clamp_ttl(u64::MAX), MAX_TTL_SECS);
        assert_eq!(clamp_ttl(60), 60);
    }
}
