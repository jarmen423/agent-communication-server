//! `backup.*` query-API ops (storage-surreal only).
//!
//! `backup.export` writes a consistent `.surql` snapshot of the running
//! hub-server's database to a path **on the hub host** (the export runs in
//! a single read transaction). It's a filesystem write, so it's gated like
//! every other write op — admin-only once `--api-admin` is configured (see
//! `query_api/authz.rs`).

use serde_json::{json, Value};

use super::super::ApiResponse;
use crate::storage::{Storage, SurrealStorage};

/// `backup.export`: `{path}` → writes the export there (must not exist).
/// Returns `{path, bytes}`.
pub(in crate::query_api) async fn backup_export(s: &dyn Storage, p: &Value) -> ApiResponse {
    let Some(storage) = s.as_any().downcast_ref::<SurrealStorage>() else {
        return ApiResponse::err("backup.export requires the SurrealDB storage backend");
    };
    let path = match p.get("path").and_then(Value::as_str) {
        Some(p) if !p.is_empty() => std::path::PathBuf::from(p),
        _ => return ApiResponse::err("missing path"),
    };
    match crate::storage::backup::export_to(&storage.db(), &path).await {
        Ok(bytes) => ApiResponse::ok(json!({
            "path": path.display().to_string(),
            "bytes": bytes,
        })),
        Err(e) => ApiResponse::err(format!("{e:#}")),
    }
}
