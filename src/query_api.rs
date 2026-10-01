//! Query API — lets CLI tools route DB operations through the running hub-server.
//!
//! Solves RocksDB single-writer lock contention: only hub-server holds the DB
//! lock. CLI tools publish requests to `hub.api.<operation>` and get JSON replies.
//!
//! Wire format: raw JSON request → raw JSON response (not Envelopes).
//! This is internal infrastructure plumbing, not agent messaging.
//!
//! The API is backend-agnostic: it dispatches through `Arc<dyn Storage>`.
//! List operations that can grow without bound (`history.query`,
//! `thread.get`, `thread.pending`) take a `limit` that defaults to
//! [`DEFAULT_LIMIT`] and is rejected above [`MAX_LIMIT`], and no reply is
//! ever larger than the server's NATS `max_payload`.

use anyhow::Result;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tracing::{debug, warn};

use crate::storage::Storage;

pub mod authz;
mod handlers;
#[cfg(feature = "storage-surreal")]
mod stats;
mod write_authz;

pub use authz::{ApiAuthz, Caller};
use handlers::*;

/// NATS subject prefix for query API.
pub const API_PREFIX: &str = "hub.api";

/// Rows returned by list operations when the request has no `limit`.
pub const DEFAULT_LIMIT: usize = 100;

/// Largest `limit` a list operation accepts; above this the request fails.
pub const MAX_LIMIT: usize = 1000;

/// Build a legacy query API subject: `hub.api.<operation>`.
pub fn subject(operation: &str) -> String {
    format!("{API_PREFIX}.{operation}")
}

/// Build a bound query API subject: `hub.api.<identity>.<operation>`.
pub fn subject_bound(identity: &str, operation: &str) -> String {
    format!("{API_PREFIX}.{identity}.{operation}")
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
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }
    }
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_else(|_| b"{\"ok\":false}".to_vec())
    }
}

/// Resolve `params.limit`: absent/null → [`DEFAULT_LIMIT`]; must be an
/// integer in `1..=MAX_LIMIT`, otherwise the request fails with a clear error.
pub fn resolve_limit(params: &Value) -> std::result::Result<usize, ApiResponse> {
    match params.get("limit") {
        None | Some(Value::Null) => Ok(DEFAULT_LIMIT),
        Some(v) => match v.as_u64() {
            Some(0) => Err(ApiResponse::err("limit must be at least 1")),
            Some(n) if n as usize <= MAX_LIMIT => Ok(n as usize),
            Some(n) => Err(ApiResponse::err(format!(
                "limit {n} exceeds the maximum of {MAX_LIMIT}; page with since/until instead"
            ))),
            None => Err(ApiResponse::err(format!(
                "limit must be a positive integer (got {v})"
            ))),
        },
    }
}

/// Serialize a response, replacing it with an error if it would exceed
/// `max_payload` (NATS rejects oversized publishes, which would otherwise
/// surface to the caller as a timeout).
pub fn encode_response(resp: &ApiResponse, max_payload: usize) -> Vec<u8> {
    let bytes = resp.to_bytes();
    if bytes.len() <= max_payload {
        return bytes;
    }
    ApiResponse::err(format!(
        "response too large ({} bytes > NATS max_payload {max_payload}); \
         request a smaller limit",
        bytes.len()
    ))
    .to_bytes()
}

/// Start the query API listener on the hub-server side (permissive mode —
/// equivalent to [`start_api_listener_with_authz`] with
/// [`ApiAuthz::permissive`]).
///
/// Subscribes to `hub.api.>` and dispatches to the storage backend.
/// Each operation is handled in a spawned task — non-blocking.
pub async fn start_api_listener(storage: Arc<dyn Storage>, nats_url: &str) -> Result<()> {
    start_api_listener_with_authz(storage, nats_url, ApiAuthz::permissive()).await
}

/// Start the query API listener with authorization config: bound subjects
/// `hub.api.<identity>.<op>` are scoped per [`ApiAuthz`]; legacy
/// `hub.api.<op>` is rejected when `authz.require_bound` is set.
pub async fn start_api_listener_with_authz(
    storage: Arc<dyn Storage>,
    nats_url: &str,
    authz: ApiAuthz,
) -> Result<()> {
    // Same env auth path as ControlPlane / HubClient (NATS_TOKEN, TLS, …).
    let opts = crate::HubConnectOptions::from_env();
    let client = crate::connect_opts::connect_with_hub_opts(nats_url, &opts).await?;
    let mut sub = client.subscribe(format!("{API_PREFIX}.>")).await?;

    debug!(require_bound = authz.require_bound, admins = ?authz.admins, "query API listening on {API_PREFIX}.>");

    while let Some(msg) = sub.next().await {
        let storage = storage.clone();
        let client = client.clone();
        let authz = authz.clone();
        let reply_subject = msg.reply.clone();
        let subject = msg.subject.to_string();

        tokio::spawn(async move {
            let resp = handle_request_authorized(&storage, &subject, &msg.payload, &authz).await;
            if let Some(reply) = reply_subject {
                let max_payload = client.server_info().max_payload;
                let bytes = encode_response(&resp, max_payload);
                if let Err(e) = client.publish(reply, bytes.into()).await {
                    warn!(error = %e, "query API: failed to publish reply");
                }
            }
        });
    }

    Ok(())
}

/// Dispatch one API request (`subject` = `hub.api.<op>`, `payload` = JSON
/// [`ApiRequest`]) to the storage backend. Public so the API can be
/// exercised without NATS. This is the permissive entry point — no caller
/// identity or authorization is applied (pre-T1 semantics).
pub async fn handle_request(
    storage: &Arc<dyn Storage>,
    subject: &str,
    payload: &[u8],
) -> ApiResponse {
    handle_request_authorized(storage, subject, payload, &ApiAuthz::permissive()).await
}

/// Dispatch one API request with caller identity + authorization.
///
/// The subject carries the caller: `hub.api.<identity>.<op>` binds the
/// caller to `<identity>` (reads are scoped, writes need admin), while
/// `hub.api.<op>` is the privileged legacy form allowed only when
/// `authz.require_bound` is off.
pub async fn handle_request_authorized(
    storage: &Arc<dyn Storage>,
    subject: &str,
    payload: &[u8],
    authz: &ApiAuthz,
) -> ApiResponse {
    let req: ApiRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return ApiResponse::err(format!("bad request: {e}")),
    };

    let (caller, op) = match authz.parse_subject(subject) {
        Ok((caller, op)) => (caller, op),
        Err(resp) => return resp,
    };
    let op = if op.is_empty() {
        req.op.as_str()
    } else {
        op.as_str()
    };

    debug!(op, ?caller, "query API request");

    // Writes are gated *before* dispatch — post-filtering cannot undo a
    // mutation the handler already applied. Non-admins may only mutate
    // waves/sessions they own (see `write_authz`).
    if authz.enforcing() {
        if let Caller::Bound(identity) = &caller {
            if ApiAuthz::is_write_op(op)
                && !authz
                    .write_allowed(storage.as_ref(), identity, op, &req.params)
                    .await
            {
                return ApiResponse::err(format!(
                    "forbidden: '{op}' — non-admin callers may only modify waves/sessions \
                     they own (admins: --api-admin)"
                ));
            }
        }
    }

    let resp = dispatch(storage, &req, op).await;
    authz
        .authorize(storage, &caller, op, &req.params, resp)
        .await
}

/// Route `req` to the storage handler for `op` (subject-derived; falls back
/// to `req.op` when the subject is not an api subject).
async fn dispatch(storage: &Arc<dyn Storage>, req: &ApiRequest, op: &str) -> ApiResponse {
    let s: &dyn Storage = storage.as_ref();
    let p = &req.params;
    match op {
        // ── Waves ──────────────────────────────────────────────
        "wave.create" => wave_create(s, p).await,
        "wave.create_task" => wave_create_task(s, p).await,
        "wave.update_status" => wave_update_status(s, p).await,
        "wave.get" => wave_get(s, p).await,
        "wave.list" => wave_list(s, p).await,
        "wave.list_tasks" => wave_list_tasks(s, p).await,
        "wave.update_task_status" => wave_update_task_status(s, p).await,
        "wave.get_task" => wave_get_task(s, p).await,
        "wave.spawn" => wave_spawn(s, p).await,
        "wave.status" => wave_status(s, p).await,
        "wave.cancel" => wave_cancel(s, p).await,

        // ── Sessions ───────────────────────────────────────────
        "session.create" => session_create(s, p).await,
        "session.update_status" => session_update_status(s, p).await,
        "session.get" => session_get(s, p).await,
        "session.list" => session_list(s, p).await,
        "session.set_backend_ctx" => session_set_backend_ctx(s, p).await,

        // ── Agents ─────────────────────────────────────────────
        "agent.find" => agent_find(s, p).await,
        "agent.get" => agent_get(s, p).await,

        // ── History / threads (bounded) ───────────────────────
        "history.query" => history_query(s, p).await,
        "thread.get" => thread_get(s, p).await,
        "thread.pending" => thread_pending(s, p).await,
        "envelope.get" => envelope_get(s, p).await,

        // ── Stats (analytics) ─────────────────────────────────
        #[cfg(feature = "storage-surreal")]
        "stats.message_rate" => stats::stats_message_rate(storage, p).await,
        #[cfg(feature = "storage-surreal")]
        "stats.latency" => stats::stats_latency(storage, p).await,
        #[cfg(feature = "storage-surreal")]
        "stats.agent_activity" => stats::stats_agent_activity(storage, p).await,
        #[cfg(feature = "storage-surreal")]
        "stats.channel_hotspots" => stats::stats_channel_hotspots(storage, p).await,
        #[cfg(feature = "storage-surreal")]
        "stats.error_rate" => stats::stats_error_rate(storage, p).await,
        #[cfg(not(feature = "storage-surreal"))]
        op if op.starts_with("stats.") => {
            ApiResponse::err(format!("{op}: analytics needs the storage-surreal feature"))
        }

        // ── Misc ───────────────────────────────────────────────
        "ping" => ApiResponse::ok(serde_json::json!({"ok": true})),

        _ => ApiResponse::err(format!("unknown operation: {op}")),
    }
}
