//! Query API handlers for records (waves, sessions, agents, history,
//! threads). Every handler goes through the `Storage` trait.

use serde_json::{json, Value};

use super::{resolve_limit, ApiResponse};
use crate::storage::{
    AgentFilter, HistoryQuery, SessionFilter, SessionRecord, Storage, WaveRecord, WaveTaskRecord,
    MAX_THREAD_DEPTH,
};

// ── Wave handlers ──────────────────────────────────────────────

pub(super) async fn wave_create(s: &dyn Storage, p: &Value) -> ApiResponse {
    let wave: WaveRecord = match serde_json::from_value(p.clone()) {
        Ok(w) => w,
        Err(e) => return ApiResponse::err(format!("bad wave record: {e}")),
    };
    match s.create_wave(wave).await {
        Ok(()) => ApiResponse::ok(json!({"created": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn wave_create_task(s: &dyn Storage, p: &Value) -> ApiResponse {
    let task: WaveTaskRecord = match serde_json::from_value(p.clone()) {
        Ok(t) => t,
        Err(e) => return ApiResponse::err(format!("bad task record: {e}")),
    };
    match s.create_wave_task(task).await {
        Ok(()) => ApiResponse::ok(json!({"created": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn wave_update_status(s: &dyn Storage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing wave_id"),
    };
    let status = match p.get("status").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ApiResponse::err("missing status"),
    };
    match s.update_wave_status(wave_id, status).await {
        Ok(()) => ApiResponse::ok(json!({"updated": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn wave_get(s: &dyn Storage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing wave_id"),
    };
    match s.get_wave(wave_id).await {
        Ok(wave) => ApiResponse::ok(json!({"wave": wave})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn wave_list(s: &dyn Storage, p: &Value) -> ApiResponse {
    let status = p.get("status").and_then(|v| v.as_str());
    match s.list_waves(status).await {
        Ok(waves) => ApiResponse::ok(json!({"waves": waves})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn wave_list_tasks(s: &dyn Storage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing wave_id"),
    };
    match s.list_wave_tasks(wave_id).await {
        Ok(tasks) => ApiResponse::ok(json!({"tasks": tasks})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn wave_update_task_status(s: &dyn Storage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return ApiResponse::err("missing wave_id"),
    };
    let task_id = match p.get("task_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => return ApiResponse::err("missing task_id"),
    };
    let status = match p.get("status").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return ApiResponse::err("missing status"),
    };
    let result = p
        .get("result")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    match s
        .update_wave_task_status(&wave_id, &task_id, &status, result.as_deref())
        .await
    {
        Ok(()) => ApiResponse::ok(json!({"updated": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn wave_get_task(s: &dyn Storage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing wave_id"),
    };
    let task_id = match p.get("task_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing task_id"),
    };
    match s.get_wave_task(wave_id, task_id).await {
        Ok(task) => ApiResponse::ok(json!({"task": task})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

// ── Session handlers ───────────────────────────────────────────

pub(super) async fn session_create(s: &dyn Storage, p: &Value) -> ApiResponse {
    let session: SessionRecord = match serde_json::from_value(p.clone()) {
        Ok(s) => s,
        Err(e) => return ApiResponse::err(format!("bad session record: {e}")),
    };
    match s.create_session(session).await {
        Ok(()) => ApiResponse::ok(json!({"created": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn session_update_status(s: &dyn Storage, p: &Value) -> ApiResponse {
    let session_id = match p.get("session_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing session_id"),
    };
    let status = match p.get("status").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ApiResponse::err("missing status"),
    };
    match s.update_session_status(session_id, status).await {
        Ok(()) => ApiResponse::ok(json!({"updated": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn session_get(s: &dyn Storage, p: &Value) -> ApiResponse {
    let session_id = match p.get("session_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing session_id"),
    };
    match s.get_session(session_id).await {
        Ok(session) => ApiResponse::ok(json!({"session": session})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(super) async fn session_list(s: &dyn Storage, p: &Value) -> ApiResponse {
    let filter: SessionFilter = match serde_json::from_value(p.clone()) {
        Ok(f) => f,
        Err(e) => return ApiResponse::err(format!("bad filter: {e}")),
    };
    match s.list_sessions(&filter).await {
        Ok(sessions) => ApiResponse::ok(json!({"sessions": sessions})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

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
