//! Query API — lets CLI tools route DB operations through the running hub-server.
//!
//! Solves RocksDB single-writer lock contention: only hub-server holds the DB
//! lock. CLI tools publish requests to `hub.api.<operation>` and get JSON replies.
//!
//! Wire format: raw JSON request → raw JSON response (not Envelopes).
//! This is internal infrastructure plumbing, not agent messaging.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, warn};
use futures_util::StreamExt;

use crate::storage::{Storage, SurrealStorage};

/// NATS subject prefix for query API.
pub const API_PREFIX: &str = "hub.api";

/// Build a query API subject: `hub.api.<operation>`.
pub fn subject(operation: &str) -> String {
    format!("{API_PREFIX}.{operation}")
}

/// Generic request envelope from CLI tools.
#[derive(Debug, Deserialize)]
pub struct ApiRequest {
    /// Operation name (matches the subject suffix).
    pub op: String,
    /// Operation-specific parameters.
    #[serde(default)]
    pub params: Value,
}

/// Generic response from hub-server.
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ApiResponse {
    pub fn ok(data: Value) -> Self {
        Self { ok: true, data: Some(data), error: None }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self { ok: false, data: None, error: Some(msg.into()) }
    }
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_else(|_| b"{\"ok\":false}".to_vec())
    }
}

/// Start the query API listener on the hub-server side.
///
/// Subscribes to `hub.api.>` and dispatches to the storage backend.
/// Each operation is handled in a spawned task — non-blocking.
pub async fn start_api_listener(storage: Arc<SurrealStorage>, nats_url: &str) -> Result<()> {
    let client = async_nats::connect(nats_url).await?;
    let mut sub = client.subscribe(format!("{API_PREFIX}.>")).await?;

    debug!("query API listening on {API_PREFIX}.>");

    while let Some(msg) = sub.next().await {
        let storage = storage.clone();
        let client = client.clone();
        let reply_subject = msg.reply.clone();
        let subject = msg.subject.to_string();

        tokio::spawn(async move {
            let resp = handle_api_request(&storage, &subject, &msg.payload).await;
            if let Some(reply) = reply_subject {
                if let Err(e) = client.publish(reply, resp.to_bytes().into()).await {
                    warn!(error = %e, "query API: failed to publish reply");
                }
            }
        });
    }

    Ok(())
}

/// Dispatch an API request to the appropriate storage method.
async fn handle_api_request(
    storage: &SurrealStorage,
    subject: &str,
    payload: &[u8],
) -> ApiResponse {
    let req: ApiRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return ApiResponse::err(format!("bad request: {e}")),
    };

    // Extract operation from subject (hub.api.<op>) or req.op
    let op = subject
        .strip_prefix(&format!("{API_PREFIX}."))
        .unwrap_or(&req.op);

    debug!(op, "query API request");

    match op {
        // ── Waves ──────────────────────────────────────────────
        "wave.create" => wave_create(storage, &req.params).await,
        "wave.create_task" => wave_create_task(storage, &req.params).await,
        "wave.update_status" => wave_update_status(storage, &req.params).await,
        "wave.get" => wave_get(storage, &req.params).await,
        "wave.list" => wave_list(storage, &req.params).await,
        "wave.list_tasks" => wave_list_tasks(storage, &req.params).await,
        "wave.update_task_status" => wave_update_task_status(storage, &req.params).await,
        "wave.get_task" => wave_get_task(storage, &req.params).await,

        // ── Sessions ───────────────────────────────────────────
        "session.create" => session_create(storage, &req.params).await,
        "session.update_status" => session_update_status(storage, &req.params).await,
        "session.get" => session_get(storage, &req.params).await,
        "session.list" => session_list(storage, &req.params).await,

        // ── Agents ─────────────────────────────────────────────
        "agent.find" => agent_find(storage, &req.params).await,

        // ── History ────────────────────────────────────────────
        "history.query" => history_query(storage, &req.params).await,

        // ── Misc ───────────────────────────────────────────────
        "ping" => ApiResponse::ok(serde_json::json!({"ok": true})),

        _ => ApiResponse::err(format!("unknown operation: {op}")),
    }
}

// ── Wave handlers ──────────────────────────────────────────────

async fn wave_create(s: &SurrealStorage, p: &Value) -> ApiResponse {
    let wave: crate::WaveRecord = match serde_json::from_value(p.clone()) {
        Ok(w) => w,
        Err(e) => return ApiResponse::err(format!("bad wave record: {e}")),
    };
    match s.create_wave(wave).await {
        Ok(()) => ApiResponse::ok(serde_json::json!({"created": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

async fn wave_create_task(s: &SurrealStorage, p: &Value) -> ApiResponse {
    let task: crate::WaveTaskRecord = match serde_json::from_value(p.clone()) {
        Ok(t) => t,
        Err(e) => return ApiResponse::err(format!("bad task record: {e}")),
    };
    match s.create_wave_task(task).await {
        Ok(()) => ApiResponse::ok(serde_json::json!({"created": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

async fn wave_update_status(s: &SurrealStorage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing wave_id"),
    };
    let status = match p.get("status").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ApiResponse::err("missing status"),
    };
    match s.update_wave_status(wave_id, status).await {
        Ok(()) => ApiResponse::ok(serde_json::json!({"updated": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

async fn wave_get(s: &SurrealStorage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing wave_id"),
    };
    match s.get_wave(wave_id).await {
        Ok(wave) => ApiResponse::ok(serde_json::json!({"wave": wave})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

async fn wave_list(s: &SurrealStorage, p: &Value) -> ApiResponse {
    let status = p.get("status").and_then(|v| v.as_str());
    match s.list_waves(status).await {
        Ok(waves) => ApiResponse::ok(serde_json::json!({"waves": waves})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

async fn wave_list_tasks(s: &SurrealStorage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing wave_id"),
    };
    match s.list_wave_tasks(wave_id).await {
        Ok(tasks) => ApiResponse::ok(serde_json::json!({"tasks": tasks})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

async fn wave_update_task_status(s: &SurrealStorage, p: &Value) -> ApiResponse {
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
    let result = p.get("result").and_then(|v| v.as_str()).map(|s| s.to_string());
    match s.update_wave_task_status(&wave_id, &task_id, &status, result.as_deref()).await {
        Ok(()) => ApiResponse::ok(serde_json::json!({"updated": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

async fn wave_get_task(s: &SurrealStorage, p: &Value) -> ApiResponse {
    let wave_id = match p.get("wave_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing wave_id"),
    };
    let task_id = match p.get("task_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing task_id"),
    };
    match s.get_wave_task(wave_id, task_id).await {
        Ok(task) => ApiResponse::ok(serde_json::json!({"task": task})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

// ── Session handlers ───────────────────────────────────────────

async fn session_create(s: &SurrealStorage, p: &Value) -> ApiResponse {
    let session: crate::SessionRecord = match serde_json::from_value(p.clone()) {
        Ok(s) => s,
        Err(e) => return ApiResponse::err(format!("bad session record: {e}")),
    };
    match s.create_session(session).await {
        Ok(()) => ApiResponse::ok(serde_json::json!({"created": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

async fn session_update_status(s: &SurrealStorage, p: &Value) -> ApiResponse {
    let session_id = match p.get("session_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing session_id"),
    };
    let status = match p.get("status").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ApiResponse::err("missing status"),
    };
    match s.update_session_status(session_id, status).await {
        Ok(()) => ApiResponse::ok(serde_json::json!({"updated": true})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

async fn session_get(s: &SurrealStorage, p: &Value) -> ApiResponse {
    let session_id = match p.get("session_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ApiResponse::err("missing session_id"),
    };
    match s.get_session(session_id).await {
        Ok(session) => ApiResponse::ok(serde_json::json!({"session": session})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

async fn session_list(s: &SurrealStorage, p: &Value) -> ApiResponse {
    let filter: crate::SessionFilter = match serde_json::from_value(p.clone()) {
        Ok(f) => f,
        Err(e) => return ApiResponse::err(format!("bad filter: {e}")),
    };
    match s.list_sessions(&filter).await {
        Ok(sessions) => ApiResponse::ok(serde_json::json!({"sessions": sessions})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

// ── Agent handlers ─────────────────────────────────────────────

async fn agent_find(s: &SurrealStorage, p: &Value) -> ApiResponse {
    let filter: crate::AgentFilter = match serde_json::from_value(p.clone()) {
        Ok(f) => f,
        Err(e) => return ApiResponse::err(format!("bad agent filter: {e}")),
    };
    match s.find_agents(&filter).await {
        Ok(agents) => ApiResponse::ok(serde_json::json!({"agents": agents})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}

// ── History handlers ───────────────────────────────────────────

async fn history_query(s: &SurrealStorage, p: &Value) -> ApiResponse {
    let query: crate::HistoryQuery = match serde_json::from_value(p.clone()) {
        Ok(q) => q,
        Err(e) => return ApiResponse::err(format!("bad history query: {e}")),
    };
    match s.query_history(&query).await {
        Ok(envelopes) => ApiResponse::ok(serde_json::json!({"envelopes": envelopes})),
        Err(e) => ApiResponse::err(e.to_string()),
    }
}
