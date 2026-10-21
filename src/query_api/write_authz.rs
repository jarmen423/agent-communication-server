//! Owner-scoped authorization for query-API **write** ops.
//!
//! T1 made every `wave.*`/`session.*` write admin-only. With T2's server-side
//! orchestration, ordinary participants must be able to drive what they own,
//! or bound-identity mode breaks the normal flow (an orchestrator spawning its
//! own wave, a worker persisting its session's `backend_ctx`). Rule:
//!
//! | op | allowed for a non-admin caller when |
//! |---|---|
//! | `wave.create` | the new wave's `orchestrator` is the caller **and** no wave with that id exists for another orchestrator |
//! | `wave.create_task`, `wave.spawn`, `wave.cancel`, `wave.update_status` | the caller orchestrates `wave_id` |
//! | `wave.update_task_status` | the caller orchestrates the wave, **or** is the task's worker and the wave is `running` (the orchestrator then applies it through its state machine) |
//! | `session.create` | the new session's `orchestrator` is the caller **and** no session with that id exists for another orchestrator |
//! | `session.update_status`, `session.set_backend_ctx` | the caller is the session's orchestrator or worker |
//! | `lock.acquire` | `holder` is absent or equals the caller (the holder always is the caller) |
//! | `lock.renew`, `lock.release` | the caller holds the lock |
//!
//! Admins (`--api-admin`) may do anything; any other write op stays admin-only.
//! The lock service re-checks holdership after the gate, so the rule also
//! holds on a permissive hub for bound callers.
//!
//! `create` ops are upserts in storage, so without the "existing id" rule a
//! caller could re-create someone else's wave/session under its own name and
//! take it over. Storage enforces the same rule atomically (the create
//! refuses to change an existing record's orchestrator), which closes the
//! check-then-write race between two concurrent creates.

use serde_json::Value;

use super::authz::ApiAuthz;
use crate::storage::Storage;

impl ApiAuthz {
    /// May the bound identity `caller` perform write `op` with `params`?
    pub async fn write_allowed(
        &self,
        storage: &dyn Storage,
        caller: &str,
        op: &str,
        params: &Value,
    ) -> bool {
        if self.is_admin(caller) {
            return true;
        }
        let param = |k: &str| params.get(k).and_then(Value::as_str);
        match op {
            "wave.create" => {
                // Atomic form nests the record under "wave"; legacy form is the bare record.
                let orchestrator = params
                    .get("wave")
                    .and_then(|w| w.get("orchestrator"))
                    .or_else(|| params.get("orchestrator"))
                    .and_then(Value::as_str);
                let wave_id = params
                    .get("wave")
                    .and_then(|w| w.get("wave_id"))
                    .or_else(|| params.get("wave_id"))
                    .and_then(Value::as_str);
                orchestrator == Some(caller)
                    && match wave_id {
                        Some(id) => owner_or_new(storage.get_wave(id).await, caller, |w| {
                            w.orchestrator.as_str()
                        }),
                        None => true, // malformed record → the handler rejects it
                    }
            }
            "wave.create_task" | "wave.spawn" | "wave.cancel" | "wave.update_status" => {
                match param("wave_id") {
                    Some(id) => orchestrates(storage, caller, id).await,
                    None => false,
                }
            }
            "wave.update_task_status" => match (param("wave_id"), param("task_id")) {
                (Some(wave_id), Some(task_id)) => {
                    orchestrates(storage, caller, wave_id).await
                        || (wave_running(storage, wave_id).await
                            && storage
                                .get_wave_task(wave_id, task_id)
                                .await
                                .ok()
                                .flatten()
                                .is_some_and(|t| t.worker == caller))
                }
                _ => false,
            },
            "session.create" => {
                param("orchestrator") == Some(caller)
                    && match param("session_id") {
                        Some(id) => owner_or_new(storage.get_session(id).await, caller, |s| {
                            s.orchestrator.as_str()
                        }),
                        None => true, // malformed record → the handler rejects it
                    }
            }
            "session.update_status" | "session.set_backend_ctx" => match param("session_id") {
                Some(id) => storage
                    .get_session(id)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|s| s.orchestrator == caller || s.worker == caller),
                None => false,
            },
            "lock.acquire" => {
                // A bound caller may only ever lock as itself; the service
                // stamps the holder from the subject either way.
                param("holder").is_none_or(|h| h == caller)
            }
            "lock.renew" | "lock.release" => match param("lock_id") {
                Some(id) => storage
                    .get_lock(id)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|l| l.holder == caller),
                None => false,
            },
            _ => false,
        }
    }
}

/// A create may proceed when the id is free or already owned by `caller`.
/// A storage error denies (fail closed).
fn owner_or_new<T>(
    existing: anyhow::Result<Option<T>>,
    caller: &str,
    owner: impl Fn(&T) -> &str,
) -> bool {
    match existing {
        Ok(None) => true,
        Ok(Some(rec)) => owner(&rec) == caller,
        Err(_) => false,
    }
}

/// A worker may only report on a spawned wave: before `wave.spawn` (or after
/// it ends) a direct storage write would bypass dependency ordering.
async fn wave_running(storage: &dyn Storage, wave_id: &str) -> bool {
    storage
        .get_wave(wave_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|w| w.status == "running")
}

async fn orchestrates(storage: &dyn Storage, caller: &str, wave_id: &str) -> bool {
    storage
        .get_wave(wave_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|w| w.orchestrator == caller)
}
