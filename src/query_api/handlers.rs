//! Query API handlers for records (waves, sessions, agents, history,
//! threads). Every handler goes through the `Storage` trait. Wave and
//! session handlers live in [`wave`] and [`session`].

use serde_json::{json, Value};

use super::{resolve_limit, ApiResponse};
use crate::storage::{AgentFilter, HistoryQuery, Storage, MAX_THREAD_DEPTH};

#[cfg(feature = "storage-surreal")]
mod backup;
mod session;
mod wave;
mod wave_orch;

#[cfg(feature = "storage-surreal")]
pub(super) use backup::backup_export;
pub(super) use session::*;
pub(super) use wave::*;
pub(super) use wave_orch::{wave_cancel, wave_spawn, wave_status};

// ── Agent handlers ─────────────────────────────────────────────

pub(super) async fn agent_find(s: &dyn Storage, p: &Value) -> ApiResponse {
    let filter: AgentFilter = match serde_json::from_value(p.clone()) {
        Ok(f) => f,
        Err(e) => return ApiResponse::err(format!("bad agent filter: {e}")),
    };
    match s.find_agents(&filter).await {
        Ok(agents) => ApiResponse::ok(json!({"agents": agents})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

// ── History / thread handlers (bounded) ────────────────────────

/// `history.query`: a `HistoryQuery`. `limit` defaults to
/// [`DEFAULT_LIMIT`](super::DEFAULT_LIMIT) and may not exceed
/// [`MAX_LIMIT`](super::MAX_LIMIT).
pub(super) async fn history_query(s: &dyn Storage, p: &Value) -> ApiResponse {
    let mut query: HistoryQuery = match serde_json::from_value(p.clone()) {
        Ok(q) => q,
        Err(e) => return ApiResponse::err(format!("bad history query: {e}")),
    };
    let limit = match resolve_limit(p) {
        Ok(n) => n,
        Err(resp) => return resp,
    };
    query.limit = Some(limit);
    match s.query_history(&query).await {
        Ok(envelopes) => ApiResponse::ok(json!({"envelopes": envelopes, "limit": limit})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

/// `thread.get`: `{root_id, limit?, max_depth?}`. Returns the full reply
/// chain below `root_id` (root first), bounded by `limit` rows and
/// `max_depth` levels (default and cap: `MAX_THREAD_DEPTH`).
pub(super) async fn thread_get(s: &dyn Storage, p: &Value) -> ApiResponse {
    let root_id = match p.get("root_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing root_id"),
    };
    let limit = match resolve_limit(p) {
        Ok(n) => n,
        Err(resp) => return resp,
    };
    let max_depth = match p.get("max_depth") {
        None | Some(Value::Null) => MAX_THREAD_DEPTH,
        Some(v) => match v.as_u64() {
            Some(d) if d as usize <= MAX_THREAD_DEPTH => d as usize,
            _ => {
                return ApiResponse::err(format!(
                    "max_depth must be an integer between 0 and {MAX_THREAD_DEPTH}"
                ))
            }
        },
    };
    match s.get_thread_bounded(root_id, max_depth, limit).await {
        Ok(thread) => ApiResponse::ok(json!({"thread": thread, "limit": limit})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

/// `thread.pending`: `{identity, limit?}`. Unanswered messages, newest first.
pub(super) async fn thread_pending(s: &dyn Storage, p: &Value) -> ApiResponse {
    let identity = match p.get("identity").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing identity"),
    };
    let limit = match resolve_limit(p) {
        Ok(n) => n,
        Err(resp) => return resp,
    };
    match s.list_pending_bounded(identity, limit).await {
        Ok(pending) => ApiResponse::ok(json!({"pending": pending, "limit": limit})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

// ── Single-record lookups ──────────────────────────────────────

pub(super) async fn envelope_get(s: &dyn Storage, p: &Value) -> ApiResponse {
    let id = match p.get("id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing id"),
    };
    match s.get_envelope(id).await {
        Ok(env) => ApiResponse::ok(json!({"envelope": env})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn agent_get(s: &dyn Storage, p: &Value) -> ApiResponse {
    let identity = match p.get("identity").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing identity"),
    };
    match s.get_agent(identity).await {
        Ok(agent) => ApiResponse::ok(json!({"agent": agent})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}
