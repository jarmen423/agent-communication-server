//! Envelope-list reads for scoped callers (`history.query`, `thread.get`).
//!
//! Read scoping filters rows the caller can't see *after* the storage query.
//! Filtering a plain `LIMIT n` result would return short (even empty) pages
//! whenever invisible rows filled the limit, so for a scoped caller the
//! query is over-fetched at [`MAX_LIMIT`] rows per page and filtered, and
//! `history.query` walks older pages (by `until`) until `limit` visible rows
//! are found, history is exhausted, or [`MAX_PAGES`] pages were read.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Value};

use super::authz::{ApiAuthz, Scope};
use super::{dispatch, resolve_limit, ApiRequest, ApiResponse, MAX_LIMIT};
use crate::storage::Storage;

/// Upper bound on storage pages read for one scoped `history.query`.
pub(super) const MAX_PAGES: usize = 8;

/// Ops whose envelope-list result is visibility-filtered.
pub(super) fn is_scoped_list(op: &str) -> bool {
    matches!(op, "history.query" | "thread.get")
}

/// Run `op` for a scoped caller and return at most `limit` rows it may see,
/// in the handler's response shape (`{<key>: [...], "limit": n}`).
pub(super) async fn fetch_visible(
    storage: &Arc<dyn Storage>,
    req: &ApiRequest,
    op: &str,
    scope: Scope<'_>,
) -> ApiResponse {
    let limit = match resolve_limit(&req.params) {
        Ok(n) => n,
        Err(resp) => return resp,
    };
    if !req.params.is_object() && !req.params.is_null() {
        return dispatch(storage, req, op).await; // let the handler report it
    }
    let key = if op == "history.query" {
        "envelopes"
    } else {
        "thread"
    };
    let pages = if op == "history.query" { MAX_PAGES } else { 1 };

    let mut page_req = req.clone();
    if page_req.params.is_null() {
        page_req.params = json!({});
    }
    page_req.params["limit"] = json!(MAX_LIMIT);

    let mut kept: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for _ in 0..pages {
        let resp = dispatch(storage, &page_req, op).await;
        if !resp.ok {
            return resp;
        }
        let rows = match resp.data {
            Some(Value::Object(mut data)) => match data.remove(key) {
                Some(Value::Array(rows)) => rows,
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        let full_page = rows.len() >= MAX_LIMIT;
        let oldest = rows.last().and_then(record_timestamp);
        for row in rows {
            if kept.len() >= limit {
                break;
            }
            let id = row.get("id").and_then(Value::as_str).unwrap_or_default();
            if ApiAuthz::envelope_visible(scope, &row) && seen.insert(id.to_string()) {
                kept.push(row);
            }
        }
        if kept.len() >= limit || !full_page {
            break;
        }
        // Next (older) page. `until` is exclusive, so step 1ns past the
        // oldest row to keep same-timestamp rows that fell past the cut
        // (duplicates are dropped by id); stop if the cursor can't move.
        let Some(next) = oldest.map(|t| t + chrono::Duration::nanoseconds(1)) else {
            break;
        };
        let next = json!(next.to_rfc3339_opts(SecondsFormat::AutoSi, true));
        if page_req.params.get("until") == Some(&next) {
            break;
        }
        page_req.params["until"] = next;
    }
    ApiResponse::ok(json!({ key: kept, "limit": limit }))
}

fn record_timestamp(row: &Value) -> Option<DateTime<Utc>> {
    row.get("timestamp")
        .and_then(Value::as_str)
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
}
