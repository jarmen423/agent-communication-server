//! `backup.*` query-API ops (storage-surreal only).
//!
//! `backup.export` writes a consistent `.surql` snapshot of the running
//! hub-server's database (one read transaction) into the operator's
//! `--backup-dir` on the hub host. Callers name a **file**, never a path:
//! the op writes to the hub host's filesystem, so it is disabled unless the
//! operator configured a directory, and it is gated like every other write
//! op (admin-only once `--api-admin` is set; see `query_api/authz.rs`).

use std::path::Path;

use serde_json::{json, Value};

use super::super::ApiResponse;
use crate::storage::{Storage, SurrealStorage};

/// Longest accepted backup file name.
const MAX_NAME_LEN: usize = 128;

/// `true` for a plain file name: `[A-Za-z0-9._-]+`, not starting with `.`
/// (no separators, no `..`, no hidden files).
pub(crate) fn valid_backup_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `backup.export`: `{name}` → writes `<backup_dir>/<name>` (must not
/// exist). Returns `{path, bytes}`.
pub(in crate::query_api) async fn backup_export(
    s: &dyn Storage,
    p: &Value,
    backup_dir: Option<&Path>,
) -> ApiResponse {
    let Some(dir) = backup_dir else {
        return ApiResponse::err(
            "backup.export is disabled: start hub-server with --backup-dir <DIR>",
        );
    };
    let Some(storage) = s.as_any().downcast_ref::<SurrealStorage>() else {
        return ApiResponse::err("backup.export requires the SurrealDB storage backend");
    };
    let name =
        match p.get("name").and_then(Value::as_str) {
            Some(n) if valid_backup_name(n) => n,
            Some(_) => return ApiResponse::err(
                "invalid name: a plain file name of [A-Za-z0-9._-] (no '/', not starting with '.')",
            ),
            None => {
                return ApiResponse::err("missing name (a file name inside the hub's --backup-dir)")
            }
        };
    let path = dir.join(name);
    match crate::storage::backup::export_to(&storage.db(), &path).await {
        Ok(bytes) => ApiResponse::ok(json!({
            "path": path.display().to_string(),
            "bytes": bytes,
        })),
        Err(e) => ApiResponse::err(format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::valid_backup_name;

    #[test]
    fn backup_names() {
        for ok in ["hub.surql", "hub-2026-10-21_0300.surql", "a"] {
            assert!(valid_backup_name(ok), "{ok}");
        }
        for bad in [
            "",
            ".hidden",
            "..",
            "../x",
            "a/b",
            "/etc/cron.d/x",
            "a b",
            "a\nb",
        ] {
            assert!(!valid_backup_name(bad), "{bad:?}");
        }
        assert!(!valid_backup_name(&"a".repeat(129)));
    }
}
