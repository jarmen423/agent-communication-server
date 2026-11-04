//! Hub API ops for the identity manifest: `manifest.put`, `remove`, `sync`,
//! `get`, `list` and `self`.
//!
//! [`handle`] is the only way in. `handle_request_authorized` calls it before
//! its write gate, so these ops never reach `dispatch` or read scoping.
//!
//! **Who may call what** (on every hub, permissive or not):
//!
//! - `manifest.self` needs a bound caller and answers about that caller only.
//! - `manifest.get` and `manifest.list` need a bound caller that is a manifest
//!   writer (`--manifest-writer`) or an API admin (`--api-admin`).
//! - Changing the manifest needs a bound manifest writer. Only a hub with no
//!   writer configured lets an API admin change it instead.
//! - A legacy `hub.api.manifest.*` caller has no identity and is always refused.
//!
//! A bound subject proves who is calling only when NATS credentials pin it to
//! that identity, so per-identity credentials are what make this gate real.
//!
//! **Answers:** `stale` is a success, so a provisioning retry is a no-op. A bad
//! record is `bad manifest record: <field>: <why>`, and one bad record fails
//! the whole of a sync with nothing applied.

use serde_json::{json, Value};

use super::{Apply, IdentityManifest, Lookup, ManifestRecord, ManifestRefused};
use crate::query_api::{resolve_limit, ApiAuthz, ApiResponse, Caller};
use crate::storage::Storage;

/// Answer one `manifest.*` op. `op` is the request's effective op, the same one
/// the rest of the query API acts on.
pub(crate) async fn handle(
    storage: &dyn Storage,
    authz: &ApiAuthz,
    caller: &Caller,
    op: &str,
    params: &Value,
) -> ApiResponse {
    let manifest = match manifest_gate(authz, caller, op) {
        Ok(manifest) => manifest,
        Err(refusal) => return refusal,
    };
    match op {
        "manifest.put" => put(manifest, storage, params).await,
        "manifest.remove" => remove(manifest, storage, params).await,
        "manifest.sync" => sync(manifest, storage, params).await,
        "manifest.get" => get(manifest, params),
        "manifest.list" => list(manifest, params),
        "manifest.self" => match caller {
            Caller::Bound(identity) => self_record(manifest, identity),
            // The gate lets only bound callers through to this op.
            Caller::Legacy => ApiResponse::err(SELF_FORBIDDEN),
        },
        _ => ApiResponse::err(format!("unknown operation: {op}")),
    }
}

const SELF_FORBIDDEN: &str =
    "forbidden: manifest.self needs a bound caller; call hub.api.<identity>.manifest.self";

const FORBIDDEN: &str =
    "forbidden: manifest ops need a manifest writer (--manifest-writer) or an API admin";

const WRITES_RESERVED: &str =
    "forbidden: this hub has a manifest writer; manifest writes are reserved to it";

/// Decide whether `caller` may run `op`, and hand back the hub's manifest.
///
/// Only the listed reads are open to an admin on a hub with a writer, so an op
/// added later is reserved to the writer until it is listed here.
fn manifest_gate<'a>(
    authz: &'a ApiAuthz,
    caller: &Caller,
    op: &str,
) -> Result<&'a IdentityManifest, ApiResponse> {
    let Some(manifest) = authz.manifest.as_deref() else {
        return Err(ApiResponse::err("manifest: not enabled on this hub"));
    };
    let refusal = match (caller, op) {
        (Caller::Bound(_), "manifest.self") => None,
        (Caller::Legacy, "manifest.self") => Some(SELF_FORBIDDEN),
        (Caller::Legacy, _) => Some(FORBIDDEN),
        (Caller::Bound(id), _) if manifest.is_writer(id) => None,
        (Caller::Bound(id), _) if !authz.is_admin(id) => Some(FORBIDDEN),
        (Caller::Bound(_), "manifest.get" | "manifest.list") => None,
        (Caller::Bound(_), _) if manifest.writer_configured() => Some(WRITES_RESERVED),
        (Caller::Bound(_), _) => None,
    };
    match refusal {
        None => Ok(manifest),
        Some(why) => Err(ApiResponse::err(why)),
    }
}

fn bad_record(why: impl std::fmt::Display) -> ApiResponse {
    ApiResponse::err(format!("bad manifest record: {why}"))
}

/// A refused write is the caller's fault; anything else is the hub's.
fn write_failed(op: &str, err: anyhow::Error) -> ApiResponse {
    match err.downcast_ref::<ManifestRefused>() {
        Some(refused) => bad_record(refused),
        None => ApiResponse::err(format!("{op}: {err}")),
    }
}

fn parse_record(value: &Value) -> Result<ManifestRecord, ApiResponse> {
    serde_json::from_value(value.clone()).map_err(|e| bad_record(format!("record: {e}")))
}

fn required_identity(params: &Value) -> Result<&str, ApiResponse> {
    params
        .get("identity")
        .and_then(Value::as_str)
        .ok_or_else(|| bad_record("identity: must be a string"))
}

fn required_rev(params: &Value) -> Result<i64, ApiResponse> {
    params
        .get("rev")
        .and_then(Value::as_i64)
        .ok_or_else(|| bad_record("rev: must be an integer"))
}

fn apply_answer(identity: &str, requested_rev: i64, outcome: Apply) -> ApiResponse {
    let (result, rev) = match outcome {
        Apply::Applied => ("applied", requested_rev),
        Apply::Stale { current_rev } => ("stale", current_rev),
    };
    ApiResponse::ok(json!({"identity": identity, "result": result, "rev": rev}))
}

async fn put(manifest: &IdentityManifest, storage: &dyn Storage, params: &Value) -> ApiResponse {
    let Some(value) = params.get("record") else {
        return bad_record("record: missing");
    };
    let rec = match parse_record(value) {
        Ok(rec) => rec,
        Err(resp) => return resp,
    };
    let (identity, rev) = (rec.identity.clone(), rec.rev);
    match manifest.put(Some(storage), rec).await {
        Ok(outcome) => apply_answer(&identity, rev, outcome),
        Err(e) => write_failed("manifest.put", e),
    }
}

async fn remove(manifest: &IdentityManifest, storage: &dyn Storage, params: &Value) -> ApiResponse {
    let (identity, rev) = match (required_identity(params), required_rev(params)) {
        (Ok(identity), Ok(rev)) => (identity, rev),
        (Err(resp), _) | (_, Err(resp)) => return resp,
    };
    match manifest.remove(Some(storage), identity, rev).await {
        Ok(outcome) => apply_answer(identity, rev, outcome),
        Err(e) => write_failed("manifest.remove", e),
    }
}

async fn sync(manifest: &IdentityManifest, storage: &dyn Storage, params: &Value) -> ApiResponse {
    let rev = match required_rev(params) {
        Ok(rev) => rev,
        Err(resp) => return resp,
    };
    let Some(values) = params.get("records").and_then(Value::as_array) else {
        return bad_record("records: must be an array");
    };
    let records = match values.iter().map(parse_record).collect::<Result<_, _>>() {
        Ok(records) => records,
        Err(resp) => return resp,
    };
    match manifest.sync(Some(storage), rev, records).await {
        Ok(report) => ApiResponse::ok(json!({
            "rev": rev,
            "applied": report.applied,
            "stale": report.stale,
            "removed": report.removed,
            "live": report.live,
        })),
        Err(e) => write_failed("manifest.sync", e),
    }
}

fn get(manifest: &IdentityManifest, params: &Value) -> ApiResponse {
    let identity = match required_identity(params) {
        Ok(identity) => identity,
        Err(resp) => return resp,
    };
    let (rev, removed, record) = match manifest.lookup(identity) {
        Lookup::Live(rec) => (Some(rec.rev), false, Some(rec)),
        Lookup::Removed { rev } => (Some(rev), true, None),
        Lookup::Unknown => (None, false, None),
    };
    ApiResponse::ok(json!({
        "identity": identity,
        "rev": rev,
        "removed": removed,
        "record": record.as_deref(),
    }))
}

fn list(manifest: &IdentityManifest, params: &Value) -> ApiResponse {
    let limit = match resolve_limit(params) {
        Ok(limit) => limit,
        Err(resp) => return resp,
    };
    let after = match params.get("after") {
        None | Some(Value::Null) => None,
        Some(Value::String(after)) => Some(after.as_str()),
        Some(_) => return ApiResponse::err("after must be a string"),
    };
    let (records, next_after) = manifest.list(after, limit);
    let records: Vec<&ManifestRecord> = records.iter().map(AsRef::as_ref).collect();
    ApiResponse::ok(json!({"records": records, "next_after": next_after}))
}

fn self_record(manifest: &IdentityManifest, identity: &str) -> ApiResponse {
    let record = manifest.get(identity);
    ApiResponse::ok(
        json!({"identity": identity, "record": record.as_deref(), "enforced": manifest.writer_configured()}),
    )
}

#[cfg(test)]
mod enforcement_tests {
    use super::*;
    #[test]
    fn self_reports_enforcement_only_with_a_writer() {
        for enabled in [false, true] {
            let manifest = IdentityManifest::new(if enabled {
                ["writer".into()].into()
            } else {
                Default::default()
            });
            let response = serde_json::to_value(self_record(&manifest, "worker")).unwrap();
            assert_eq!(response["data"]["enforced"], enabled);
        }
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    const WRITES: [&str; 3] = ["manifest.put", "manifest.remove", "manifest.sync"];
    const READS: [&str; 2] = ["manifest.get", "manifest.list"];

    /// A hub whose admins are `admin` and `both`, with the given writers.
    fn hub(writers: &[&str]) -> ApiAuthz {
        ApiAuthz {
            require_bound: true,
            admins: BTreeSet::from(["admin".to_string(), "both".to_string()]),
            manifest: Some(Arc::new(IdentityManifest::new(
                writers.iter().map(|w| w.to_string()).collect(),
            ))),
            ..Default::default()
        }
    }

    fn bound(id: &str) -> Caller {
        Caller::Bound(id.to_string())
    }

    fn refusal(authz: &ApiAuthz, caller: &Caller, op: &str) -> Option<String> {
        manifest_gate(authz, caller, op)
            .err()
            .map(|r| r.error.unwrap())
    }

    #[test]
    fn a_writer_hub_reserves_writes_to_its_writer() {
        let hub = hub(&["writer"]);
        for op in WRITES {
            assert_eq!(
                refusal(&hub, &bound("admin"), op).as_deref(),
                Some(WRITES_RESERVED),
                "{op}"
            );
            assert_eq!(refusal(&hub, &bound("writer"), op), None, "{op}");
        }
    }

    #[test]
    fn a_writer_hub_lets_admin_and_writer_read() {
        let hub = hub(&["writer"]);
        for op in READS {
            assert_eq!(refusal(&hub, &bound("admin"), op), None, "{op}");
            assert_eq!(refusal(&hub, &bound("writer"), op), None, "{op}");
        }
    }

    #[test]
    fn the_refusal_does_not_name_the_writer() {
        let hub = hub(&["secret-writer"]);
        let why = refusal(&hub, &bound("admin"), "manifest.put").unwrap();
        assert!(!why.contains("secret-writer"), "{why}");
    }

    #[test]
    fn an_admin_that_is_also_the_writer_may_write() {
        let hub = hub(&["both"]);
        for op in WRITES {
            assert_eq!(refusal(&hub, &bound("both"), op), None, "{op}");
        }
    }

    /// An op nobody listed as a read stays with the writer.
    #[test]
    fn an_unlisted_op_is_not_open_to_an_admin_on_a_writer_hub() {
        let hub = hub(&["writer"]);
        assert_eq!(
            refusal(&hub, &bound("admin"), "manifest.nope").as_deref(),
            Some(WRITES_RESERVED)
        );
    }

    #[test]
    fn a_hub_without_a_writer_lets_admins_write_and_read() {
        let hub = hub(&[]);
        for op in WRITES.into_iter().chain(READS) {
            assert_eq!(refusal(&hub, &bound("admin"), op), None, "{op}");
        }
    }

    #[test]
    fn other_bound_callers_get_the_general_refusal_in_both_modes() {
        for hub in [hub(&["writer"]), hub(&[])] {
            for op in WRITES.into_iter().chain(READS) {
                assert_eq!(
                    refusal(&hub, &bound("someone"), op).as_deref(),
                    Some(FORBIDDEN),
                    "{op}"
                );
            }
        }
    }

    #[test]
    fn self_is_open_to_any_bound_caller_in_both_modes() {
        for hub in [hub(&["writer"]), hub(&[])] {
            for id in ["someone", "admin", "writer"] {
                assert_eq!(refusal(&hub, &bound(id), "manifest.self"), None, "{id}");
            }
        }
    }

    #[test]
    fn a_legacy_caller_is_refused_every_op_in_both_modes() {
        for hub in [hub(&["writer"]), hub(&[])] {
            for op in WRITES.into_iter().chain(READS) {
                assert_eq!(
                    refusal(&hub, &Caller::Legacy, op).as_deref(),
                    Some(FORBIDDEN),
                    "{op}"
                );
            }
            assert_eq!(
                refusal(&hub, &Caller::Legacy, "manifest.self").as_deref(),
                Some(SELF_FORBIDDEN)
            );
        }
    }
}
