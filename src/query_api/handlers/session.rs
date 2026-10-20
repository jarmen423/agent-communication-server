//! Session query-API handlers (`session.*`).

use chrono::Utc;
use serde_json::{json, Value};

use crate::query_api::ApiResponse;
use crate::storage::{SessionFilter, SessionRecord, Storage};

/// `session.set_backend_ctx`: `{session_id, backend_ctx}` — persist the
/// backend-native session reference (claude session id, codex thread id,
/// cursor agent id, …) on the session record so a restarted worker can
/// resume a session it no longer holds in memory. `session.get` returns
/// the stored value.
pub(in crate::query_api) async fn session_set_backend_ctx(
    s: &dyn Storage,
    p: &Value,
) -> ApiResponse {
    let session_id = match p.get("session_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing session_id"),
    };
    let Some(backend_ctx) = p.get("backend_ctx") else {
        return ApiResponse::err("missing backend_ctx");
    };
    if !backend_ctx.is_object() {
        return ApiResponse::err("backend_ctx must be a JSON object");
    }
    let mut session = match s.get_session(session_id).await {
        Ok(Some(session)) => session,
        Ok(None) => return ApiResponse::err(format!("session '{session_id}' not found")),
        Err(e) => return ApiResponse::err(e.to_string()),
    };
    session.backend_ctx = Some(backend_ctx.clone());
    session.updated_at = Utc::now();
    match s.create_session(session).await {
        Ok(()) => ApiResponse::ok(json!({"updated": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

// ── Session handlers ───────────────────────────────────────────

pub(in crate::query_api) async fn session_create(s: &dyn Storage, p: &Value) -> ApiResponse {
    let session: SessionRecord = match serde_json::from_value(p.clone()) {
        Ok(s) => s,
        Err(e) => return ApiResponse::err(format!("bad session record: {e}")),
    };
    match s.create_session(session).await {
        Ok(()) => ApiResponse::ok(json!({"created": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(in crate::query_api) async fn session_update_status(s: &dyn Storage, p: &Value) -> ApiResponse {
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

pub(in crate::query_api) async fn session_get(s: &dyn Storage, p: &Value) -> ApiResponse {
    let session_id = match p.get("session_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing session_id"),
    };
    match s.get_session(session_id).await {
        Ok(session) => ApiResponse::ok(json!({"session": session})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

pub(in crate::query_api) async fn session_list(s: &dyn Storage, p: &Value) -> ApiResponse {
    let filter: SessionFilter = match serde_json::from_value(p.clone()) {
        Ok(f) => f,
        Err(e) => return ApiResponse::err(format!("bad filter: {e}")),
    };
    match s.list_sessions(&filter).await {
        Ok(sessions) => ApiResponse::ok(json!({"sessions": sessions})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}
