//! Query API wire types and subject helpers.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// NATS subject prefix for query API.
pub const API_PREFIX: &str = "hub.api";

/// Reserved API namespace tokens: a caller identity must not collide with
/// the first token of a legacy operation.
pub const OP_NAMESPACES: &[&str] = &[
    "wave", "session", "agent", "history", "thread", "envelope", "stats", "ping", "backup", "lock",
];

/// Build a legacy query API subject: `hub.api.<operation>`.
pub fn subject(operation: &str) -> String {
    format!("{API_PREFIX}.{operation}")
}

/// Build a bound query API subject: `hub.api.<identity>.<operation>`.
pub fn subject_bound(identity: &str, operation: &str) -> String {
    format!("{API_PREFIX}.{identity}.{operation}")
}

/// Generic request envelope from CLI tools.
#[derive(Debug, Clone, Deserialize)]
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
