//! Wave orchestration ops: `wave.spawn`, `wave.status`, `wave.cancel`.
//! Spawn and cancel forward to the hub-server wave orchestrator.

use serde_json::Value;
use tokio::sync::oneshot;

use crate::orchestrator::{
    clamp_timeout_secs, orchestrator_handle, snapshot_json, OrchCommand, DEFAULT_TIMEOUT_SECS,
    MAX_TIMEOUT_SECS, MIN_TIMEOUT_SECS,
};
use crate::query_api::ApiResponse;
use crate::storage::Storage;

type Reply = oneshot::Sender<Result<Value, String>>;

/// Send a command to the orchestrator and wait for its answer. `None`
/// when no orchestrator runs in this process.
pub(super) async fn ask_orchestrator(
    make: impl FnOnce(Reply) -> OrchCommand,
) -> Option<ApiResponse> {
    let handle = orchestrator_handle()?;
    let (tx, rx) = oneshot::channel();
    if handle.send(make(tx)).await.is_err() {
        return Some(ApiResponse::err("wave orchestrator is unavailable"));
    }
    Some(match rx.await {
        Ok(Ok(data)) => ApiResponse::ok(data),
        Ok(Err(e)) => ApiResponse::err(e),
        Err(_) => ApiResponse::err("wave orchestrator dropped the request"),
    })
}

/// `timeout_secs` from the request: absent/null → the default; otherwise a
/// non-negative integer, clamped to `MIN_TIMEOUT_SECS ..= MAX_TIMEOUT_SECS`
/// (an unclamped `u64::MAX` used to overflow the deadline and panic the
/// orchestrator).
pub(super) fn resolve_timeout_secs(p: &Value) -> Result<u64, ApiResponse> {
    match p.get("timeout_secs") {
        None | Some(Value::Null) => Ok(DEFAULT_TIMEOUT_SECS),
        Some(v) => match v.as_u64() {
            Some(n) => Ok(clamp_timeout_secs(n)),
            None => Err(ApiResponse::err(format!(
                "timeout_secs must be a non-negative integer (got {v}); \
                 it is clamped to {MIN_TIMEOUT_SECS}..={MAX_TIMEOUT_SECS}"
            ))),
        },
    }
}

/// `wave.spawn`: `{wave_id, timeout_secs?}` — hand the wave to the
/// hub-server orchestrator, which dispatches ready tasks and drives the
/// wave to a terminal status. Returns the wave snapshot.
pub(in crate::query_api) async fn wave_spawn(_s: &dyn Storage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return ApiResponse::err("missing wave_id"),
    };
    let timeout_secs = match resolve_timeout_secs(p) {
        Ok(t) => t,
        Err(resp) => return resp,
    };
    ask_orchestrator(|reply| OrchCommand::Spawn {
        wave_id,
        timeout_secs,
        reply,
    })
    .await
    .unwrap_or_else(|| {
        ApiResponse::err(
            "wave orchestration requires a running hub-server with storage \
             (the wave orchestrator is not active)",
        )
    })
}

/// `wave.status`: `{wave_id}` — wave record, tasks, and a summary
/// (counts per status + merge gate). Pure storage read.
pub(in crate::query_api) async fn wave_status(s: &dyn Storage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing wave_id"),
    };
    let wave = match s.get_wave(wave_id).await {
        Ok(Some(w)) => w,
        Ok(None) => return ApiResponse::err(format!("wave '{wave_id}' not found")),
        Err(e) => return ApiResponse::err(e.to_string()),
    };
    let tasks = match s.list_wave_tasks(wave_id).await {
        Ok(t) => t,
        Err(e) => return ApiResponse::err(e.to_string()),
    };
    ApiResponse::ok(snapshot_json(&wave, &tasks))
}

/// `wave.cancel`: `{wave_id}` — the orchestrator cancels every non-terminal
/// task (running workers get the §4.2 cancel DM) and marks the wave
/// `cancelled`. A terminal wave (`completed`, `failed`, `cancelled`) is final.
pub(in crate::query_api) async fn wave_cancel(_s: &dyn Storage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return ApiResponse::err("missing wave_id"),
    };
    ask_orchestrator(|reply| OrchCommand::Cancel { wave_id, reply })
        .await
        .unwrap_or_else(|| {
            ApiResponse::err(
                "wave cancellation requires a running hub-server with storage \
                 (the wave orchestrator is not active)",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn timeout_defaults_and_clamps() {
        assert_eq!(
            resolve_timeout_secs(&json!({})).ok(),
            Some(DEFAULT_TIMEOUT_SECS)
        );
        assert_eq!(
            resolve_timeout_secs(&json!({"timeout_secs": null})).ok(),
            Some(DEFAULT_TIMEOUT_SECS)
        );
        assert_eq!(
            resolve_timeout_secs(&json!({"timeout_secs": 0})).ok(),
            Some(MIN_TIMEOUT_SECS)
        );
        assert_eq!(
            resolve_timeout_secs(&json!({"timeout_secs": 90})).ok(),
            Some(90)
        );
        assert_eq!(
            resolve_timeout_secs(&json!({"timeout_secs": u64::MAX})).ok(),
            Some(MAX_TIMEOUT_SECS)
        );
    }

    #[test]
    fn timeout_rejects_non_integers() {
        for bad in [json!(-1), json!(1.5), json!("60"), json!(true)] {
            assert!(
                resolve_timeout_secs(&json!({"timeout_secs": bad})).is_err(),
                "{bad}"
            );
        }
    }
}
