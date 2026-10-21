//! `lock.*` operation handlers — validation and storage effects.
//!
//! `acquire`, `renew` and `release` run under the service's mutex (called
//! from [`LockService::handle_write`]); `list`/`get` are plain reads the
//! query API dispatches without a service.

use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{check_holder, resolve_holder, LockService};
use crate::query_api::{resolve_limit, ApiResponse, Caller};
use crate::storage::{LockRecord, Storage};
use nats_hub_client::locks::{clamp_ttl, lock_channel, normalize_path, paths_conflict, DEFAULT_TTL_SECS};

/// Field limits — generous but bounded so one call can't smuggle a huge
/// document into a replicated record.
const MAX_SCOPE_LEN: usize = 256;
const MAX_PATH_LEN: usize = 512;
const MAX_PATHS: usize = 128;
const MAX_REASON_LEN: usize = 1024;
const MAX_TASK_ID_LEN: usize = 128;

/// Live (unexpired) locks — rows past `expires_at` are dead even if the
/// sweep hasn't deleted them yet.
pub(crate) async fn live_locks(
    storage: &dyn Storage,
    scope: Option<&str>,
) -> Result<Vec<LockRecord>, ApiResponse> {
    let now = Utc::now();
    storage
        .list_locks(scope)
        .await
        .map(|locks| locks.into_iter().filter(|l| l.expires_at > now).collect())
        .map_err(|e| ApiResponse::err(e.to_string()))
}

/// `lock.list {scope?, holder?, limit?}` — live locks. When `scope` is
/// given the response names its `lock.<hash>` event channel so waiters
/// can subscribe without computing the hash.
pub(super) async fn list(storage: &dyn Storage, p: &Value) -> ApiResponse {
    let scope = p.get("scope").and_then(Value::as_str);
    let holder = p.get("holder").and_then(Value::as_str);
    let limit = match resolve_limit(p) {
        Ok(n) => n,
        Err(resp) => return resp,
    };
    let mut locks = match live_locks(storage, scope).await {
        Ok(l) => l,
        Err(resp) => return resp,
    };
    if let Some(h) = holder {
        locks.retain(|l| l.holder == h);
    }
    locks.truncate(limit);
    let mut data = json!({"locks": locks, "limit": limit});
    if let Some(s) = scope {
        data["channel"] = json!(lock_channel(s));
    }
    ApiResponse::ok(data)
}

/// `lock.get {lock_id}` — one live lock; gone or expired → "not found".
pub(super) async fn get(storage: &dyn Storage, p: &Value) -> ApiResponse {
    let lock_id = match p.get("lock_id").and_then(Value::as_str) {
        Some(id) => id,
        None => return ApiResponse::err("missing lock_id"),
    };
    let now = Utc::now();
    match storage.get_lock(lock_id).await {
        Ok(Some(lock)) if lock.expires_at > now => ApiResponse::ok(json!({
            "lock": lock,
            "channel": lock_channel(&lock.scope),
        })),
        Ok(_) => ApiResponse::err(format!("lock '{lock_id}' not found")),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

impl LockService {
    /// Serialized entry point for the mutating ops (called from
    /// `dispatch_op` when a service is installed; public so integration
    /// tests can drive a service without installing the global handle).
    pub async fn handle_write(
        &self,
        caller: &Caller,
        admin: bool,
        op: &str,
        p: &Value,
    ) -> ApiResponse {
        // One mutex serializes every acquire/renew/release inside
        // hub-server — the only process allowed to write — so concurrent
        // acquires of overlapping paths can never both win.
        let _guard = self.inner.lock().await;
        match op {
            "lock.acquire" => self.acquire(caller, p).await,
            "lock.renew" => self.renew(caller, admin, p).await,
            "lock.release" => self.release(caller, admin, p).await,
            _ => unreachable!("handle_write called for {op}"),
        }
    }

    /// `lock.acquire {scope, paths, ttl_secs?, reason?, task_id?}` —
    /// all-or-nothing. On conflict: `{granted: false, conflicts: [...]}`
    /// (ok — the refusal carries data, it is not a server error).
    async fn acquire(&self, caller: &Caller, p: &Value) -> ApiResponse {
        let holder = match resolve_holder(caller, p) {
            Ok(h) => h,
            Err(resp) => return resp,
        };
        let scope = match p.get("scope").and_then(Value::as_str) {
            Some(s) if !s.trim().is_empty() && s.len() <= MAX_SCOPE_LEN => s.trim().to_string(),
            Some(s) => {
                return ApiResponse::err(format!(
                    "scope must be a non-empty string of at most {MAX_SCOPE_LEN} chars \
                     (got {} chars)",
                    s.len()
                ))
            }
            None => return ApiResponse::err("missing scope"),
        };
        let raw_paths = match p.get("paths").and_then(Value::as_array) {
            Some(arr) if !arr.is_empty() && arr.len() <= MAX_PATHS => arr,
            Some(arr) => {
                return ApiResponse::err(format!(
                    "paths must name 1..={MAX_PATHS} entries (got {})",
                    arr.len()
                ))
            }
            None => return ApiResponse::err("missing paths"),
        };
        let mut paths: Vec<String> = Vec::with_capacity(raw_paths.len());
        for v in raw_paths {
            let Some(raw) = v.as_str() else {
                return ApiResponse::err("paths entries must be strings");
            };
            match normalize_path(raw) {
                Some(p) if p.len() <= MAX_PATH_LEN => {
                    if !paths.contains(&p) {
                        paths.push(p);
                    }
                }
                Some(_) => {
                    return ApiResponse::err(format!(
                        "path '{raw}' exceeds {MAX_PATH_LEN} chars"
                    ))
                }
                None => return ApiResponse::err(format!("path '{raw}' is empty")),
            }
        }
        let reason = match bounded_str(p, "reason", MAX_REASON_LEN) {
            Ok(r) => r,
            Err(resp) => return resp,
        };
        let task_id = match bounded_str(p, "task_id", MAX_TASK_ID_LEN) {
            Ok(t) => t,
            Err(resp) => return resp,
        };
        let ttl_secs = match p.get("ttl_secs") {
            None | Some(Value::Null) => DEFAULT_TTL_SECS,
            Some(v) => match v.as_u64() {
                Some(n) => clamp_ttl(n),
                None => return ApiResponse::err("ttl_secs must be a positive integer"),
            },
        };

        // Conflict check against live locks. Locks held by this same
        // caller never block a re-acquire — an agent coordinating with
        // itself is free to stack locks.
        let conflicts: Vec<LockRecord> = match live_locks(self.storage.as_ref(), Some(&scope)).await
        {
            Ok(live) => live
                .into_iter()
                .filter(|l| l.holder != holder && paths_conflict(&scope, &paths, l))
                .collect(),
            Err(resp) => return resp,
        };
        if !conflicts.is_empty() {
            return ApiResponse::ok(json!({
                "granted": false,
                "conflicts": conflicts,
                "channel": lock_channel(&scope),
            }));
        }

        let now = Utc::now();
        let lock = LockRecord {
            lock_id: Uuid::new_v4().to_string(),
            scope: scope.clone(),
            paths,
            holder: holder.clone(),
            reason,
            task_id,
            ttl_secs,
            created_at: now,
            expires_at: now + chrono::Duration::seconds(ttl_secs as i64),
        };
        if let Err(e) = self.storage.create_lock(lock.clone()).await {
            return ApiResponse::err(format!("failed to store lock: {e}"));
        }
        self.publish_event(
            "lock.acquired",
            json!({"lock_id": lock.lock_id, "scope": lock.scope, "holder": lock.holder,
                   "paths": lock.paths, "reason": lock.reason, "task_id": lock.task_id,
                   "expires_at": lock.expires_at}),
        )
        .await;
        ApiResponse::ok(json!({
            "granted": true,
            "lock": lock,
            "channel": lock_channel(&scope),
        }))
    }

    /// `lock.renew {lock_id, ttl_secs?}` — holder (or admin) only. The new
    /// expiry is `now + ttl` where `ttl` defaults to the lock's own TTL.
    async fn renew(&self, caller: &Caller, admin: bool, p: &Value) -> ApiResponse {
        let lock_id = match p.get("lock_id").and_then(Value::as_str) {
            Some(id) => id,
            None => return ApiResponse::err("missing lock_id"),
        };
        let lock = match self.storage.get_lock(lock_id).await {
            Ok(Some(l)) if l.expires_at > Utc::now() => l,
            Ok(_) => return ApiResponse::err(format!("lock '{lock_id}' not found or expired")),
            Err(e) => return ApiResponse::err(e.to_string()),
        };
        if let Err(resp) = check_holder(caller, admin, p, &lock.holder) {
            return resp;
        }
        let ttl_secs = match p.get("ttl_secs") {
            None | Some(Value::Null) => lock.ttl_secs,
            Some(v) => match v.as_u64() {
                Some(n) => clamp_ttl(n),
                None => return ApiResponse::err("ttl_secs must be a positive integer"),
            },
        };
        let expires_at = Utc::now() + chrono::Duration::seconds(ttl_secs as i64);
        match self
            .storage
            .renew_lock(lock_id, ttl_secs, expires_at)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                return ApiResponse::err(format!("lock '{lock_id}' not found"))
            }
            Err(e) => return ApiResponse::err(e.to_string()),
        }
        let mut lock = lock;
        lock.ttl_secs = ttl_secs;
        lock.expires_at = expires_at;
        ApiResponse::ok(json!({"lock": lock}))
    }

    /// `lock.release {lock_id, force?}` — holder (or admin) only. `force`
    /// marks an admin's release of someone else's lock in the event.
    async fn release(&self, caller: &Caller, admin: bool, p: &Value) -> ApiResponse {
        let lock_id = match p.get("lock_id").and_then(Value::as_str) {
            Some(id) => id,
            None => return ApiResponse::err("missing lock_id"),
        };
        let lock = match self.storage.get_lock(lock_id).await {
            Ok(Some(l)) => l,
            Ok(None) => return ApiResponse::err(format!("lock '{lock_id}' not found")),
            Err(e) => return ApiResponse::err(e.to_string()),
        };
        if let Err(resp) = check_holder(caller, admin, p, &lock.holder) {
            return resp;
        }
        match self.storage.delete_lock(lock_id).await {
            Ok(true) => {}
            Ok(false) => {
                return ApiResponse::err(format!("lock '{lock_id}' not found"))
            }
            Err(e) => return ApiResponse::err(e.to_string()),
        }
        let forced = admin
            && resolve_holder(caller, p)
                .map(|id| id != lock.holder)
                .unwrap_or(false);
        self.publish_event(
            "lock.released",
            json!({"lock_id": lock.lock_id, "scope": lock.scope, "holder": lock.holder,
                   "paths": lock.paths, "forced": forced}),
        )
        .await;
        ApiResponse::ok(json!({"released": lock_id}))
    }
}

/// An optional string param with a length bound.
fn bounded_str(p: &Value, key: &str, max: usize) -> Result<Option<String>, ApiResponse> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_str() {
            Some(s) if s.len() <= max => Ok(Some(s.to_string())),
            Some(_) => Err(ApiResponse::err(format!("{key} exceeds {max} chars"))),
            None => Err(ApiResponse::err(format!("{key} must be a string"))),
        },
    }
}
