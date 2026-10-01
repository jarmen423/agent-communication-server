//! Owner-scoped authorization for query-API **write** ops.
//!
//! T1 made every `wave.*`/`session.*` write admin-only. With T2's server-side
//! orchestration, ordinary participants must be able to drive what they own,
//! or bound-identity mode breaks the normal flow (an orchestrator spawning its
//! own wave, a worker persisting its session's `backend_ctx`). Rule:
//!
//! | op | allowed for a non-admin caller when |
//! |---|---|
//! | `wave.create` | the new wave's `orchestrator` is the caller |
//! | `wave.create_task`, `wave.spawn`, `wave.cancel`, `wave.update_status` | the caller orchestrates `wave_id` |
//! | `wave.update_task_status` | the caller orchestrates the wave **or** is the task's worker |
//! | `session.create` | the new session's `orchestrator` is the caller |
//! | `session.update_status`, `session.set_backend_ctx` | the caller is the session's orchestrator or worker |
//!
//! Admins (`--api-admin`) may do anything; any other write op stays admin-only.

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
                orchestrator == Some(caller)
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
                        || storage
                            .get_wave_task(wave_id, task_id)
                            .await
                            .ok()
                            .flatten()
                            .is_some_and(|t| t.worker == caller)
                }
                _ => false,
            },
            "session.create" => param("orchestrator") == Some(caller),
            "session.update_status" | "session.set_backend_ctx" => match param("session_id") {
                Some(id) => storage
                    .get_session(id)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|s| s.orchestrator == caller || s.worker == caller),
                None => false,
            },
            _ => false,
        }
    }
}

async fn orchestrates(storage: &dyn Storage, caller: &str, wave_id: &str) -> bool {
    storage
        .get_wave(wave_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|w| w.orchestrator == caller)
}
