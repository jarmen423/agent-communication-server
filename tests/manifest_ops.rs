//! Identity manifest API ops: authorization and the JSON shapes, both directly
//! through `handle_request_authorized` and live against `with_stack.sh` (which
//! always runs hub-server with `--manifest-writer test-manifest`).
//!
//! Tests in this file share one hub and run in parallel, so each uses its own
//! identities. A sync tombstones live slots below its snapshot rev, so the one
//! successful sync stays at low revs and every other test writes above them.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use nats_hub::manifest::IdentityManifest;
use nats_hub::query_api::{handle_request_authorized, ApiAuthz, ApiResponse};
use nats_hub::{Storage, SurrealStorage};
use serde_json::{json, Value};

const WRITER: &str = "test-manifest";
const FORBIDDEN: &str =
    "forbidden: manifest ops need a manifest writer (--manifest-writer) or an API admin";

// ── Directly through the query API (no NATS) ───────────────────────

async fn storage() -> Arc<dyn Storage> {
    let s = SurrealStorage::connect_memory().await.unwrap();
    s.migrate().await.unwrap();
    Arc::new(s)
}

fn authz(require_bound: bool) -> ApiAuthz {
    ApiAuthz {
        require_bound,
        admins: if require_bound {
            BTreeSet::from(["boss".to_string()])
        } else {
            BTreeSet::new()
        },
        manifest: Some(Arc::new(IdentityManifest::new(BTreeSet::from([
            "writer".to_string()
        ])))),
        ..Default::default()
    }
}

async fn direct(
    s: &Arc<dyn Storage>,
    authz: &ApiAuthz,
    subject: &str,
    op: &str,
    params: Value,
) -> ApiResponse {
    let body = serde_json::to_vec(&json!({"op": op, "params": params})).unwrap();
    handle_request_authorized(s, subject, &body, authz).await
}

fn user_record(identity: &str, user: &str, rev: i64) -> Value {
    json!({"identity": identity, "owner_kind": "user", "user": user, "rev": rev})
}

#[tokio::test]
async fn a_hub_without_a_manifest_says_so() {
    let s = storage().await;
    let resp = direct(
        &s,
        &ApiAuthz::permissive(),
        "hub.api.alice.manifest.self",
        "manifest.self",
        json!({}),
    )
    .await;
    assert!(!resp.ok);
    assert_eq!(
        resp.error.as_deref(),
        Some("manifest: not enabled on this hub")
    );
}

/// The subject decides the op whenever it names one; the body can't steer a
/// request into or out of the manifest gate.
#[tokio::test]
async fn the_request_body_cannot_steer_the_gate() {
    let s = storage().await;
    let a = authz(false);
    let put = json!({"record": user_record("steered", "alice", 1)});

    // A manifest subject with a harmless body op is still gated as a put.
    let resp = direct(&s, &a, "hub.api.alice.manifest.put", "ping", put.clone()).await;
    assert_eq!(resp.error.as_deref(), Some(FORBIDDEN));

    // A manifest body op on another subject runs that subject's op.
    let resp = direct(&s, &a, "hub.api.writer.ping", "manifest.put", put.clone()).await;
    assert!(resp.ok, "{resp:?}");
    let manifest = a.manifest.as_ref().unwrap();
    assert!(manifest.get("steered").is_none(), "the put must not apply");

    // A subject outside hub.api carries no caller, so the body op is refused.
    let resp = direct(&s, &a, "not.an.api.subject", "manifest.put", put).await;
    assert_eq!(resp.error.as_deref(), Some(FORBIDDEN));
    assert!(manifest.get("steered").is_none());
}

#[tokio::test]
async fn gate_holds_on_a_permissive_hub_and_under_require_bound() {
    let s = storage().await;
    let put = json!({"record": user_record("gated", "alice", 1)});

    let permissive = authz(false);
    let legacy = direct(
        &s,
        &permissive,
        "hub.api.manifest.put",
        "manifest.put",
        put.clone(),
    )
    .await;
    assert_eq!(legacy.error.as_deref(), Some(FORBIDDEN));
    let legacy = direct(
        &s,
        &permissive,
        "hub.api.manifest.self",
        "manifest.self",
        json!({}),
    )
    .await;
    assert!(legacy.error.unwrap().contains("needs a bound caller"));

    let bound = authz(true);
    let legacy = direct(
        &s,
        &bound,
        "hub.api.manifest.put",
        "manifest.put",
        put.clone(),
    )
    .await;
    assert!(legacy
        .error
        .unwrap()
        .contains("legacy hub.api.<op> rejected"));

    let resp = direct(&s, &bound, "hub.api.boss.manifest.put", "manifest.put", put).await;
    assert!(resp.ok, "an API admin may write: {resp:?}");
    assert_eq!(resp.data.unwrap()["result"], "applied");
}

/// Being a manifest writer grants nothing beyond the manifest.
#[tokio::test]
async fn a_writer_gets_no_other_rights() {
    let s = storage().await;
    let a = authz(true);
    let resp = direct(
        &s,
        &a,
        "hub.api.writer.backup.export",
        "backup.export",
        json!({"name": "x.surql"}),
    )
    .await;
    assert!(resp.error.unwrap().contains("forbidden"));
}

#[tokio::test]
async fn unknown_manifest_op_is_refused_after_the_gate() {
    let s = storage().await;
    let a = authz(false);
    let resp = direct(&s, &a, "hub.api.alice.manifest.nope", "x", json!({})).await;
    assert_eq!(resp.error.as_deref(), Some(FORBIDDEN));
    let resp = direct(&s, &a, "hub.api.writer.manifest.nope", "x", json!({})).await;
    assert_eq!(
        resp.error.as_deref(),
        Some("unknown operation: manifest.nope")
    );
}

// ── Live against with_stack.sh ─────────────────────────────────────

fn stack_url() -> Option<String> {
    if std::env::var("NATS_HUB_TEST_MANIFEST").ok().as_deref() != Some("1") {
        eprintln!("Skipping — needs scripts/dev/with_stack.sh (NATS_HUB_TEST_MANIFEST=1)");
        return None;
    }
    Some(std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into()))
}

fn require_bound() -> bool {
    std::env::var("NATS_HUB_REQUIRE_BOUND").is_ok()
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// A rev above every other this suite uses, and above the sync test's snapshot.
fn next_rev() -> i64 {
    static NEXT: AtomicI64 = AtomicI64::new(1_000_000);
    NEXT.fetch_add(10, Ordering::SeqCst)
}

async fn raw(nc: &async_nats::Client, subject: String, op: &str, params: Value) -> Value {
    let body = serde_json::to_vec(&json!({"op": op, "params": params})).unwrap();
    let msg = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        nc.request(subject, body.into()),
    )
    .await
    .expect("no reply in time")
    .expect("request failed");
    serde_json::from_slice(&msg.payload).unwrap()
}

/// Call `op` as `caller` on the bound subject.
async fn call(nc: &async_nats::Client, caller: &str, op: &str, params: Value) -> Value {
    raw(nc, format!("hub.api.{caller}.{op}"), op, params).await
}

/// Call `op` as the writer and insist that it succeeds.
async fn write_ok(nc: &async_nats::Client, op: &str, params: Value) -> Value {
    let resp = call(nc, WRITER, op, params).await;
    assert_eq!(resp["ok"], true, "{op}: {resp}");
    resp["data"].clone()
}

fn error_of(resp: &Value) -> &str {
    assert_eq!(resp["ok"], false, "expected a refusal: {resp}");
    resp["error"].as_str().unwrap()
}

macro_rules! live {
    () => {
        match stack_url() {
            Some(url) => async_nats::connect(&url).await.unwrap(),
            None => return,
        }
    };
}

#[tokio::test]
async fn put_get_list_remove_round_trip() {
    let nc = live!();
    let id = unique("rt");
    let rev = next_rev();

    let unknown = write_ok(&nc, "manifest.get", json!({"identity": id})).await;
    assert_eq!(
        unknown,
        json!({"identity": id, "rev": null, "removed": false, "record": null})
    );

    let record = json!({
        "identity": id, "owner_kind": "user", "user": "alice",
        "user_id": "6f9619ff-8b86-4011-b42d-00c04fc964ff", "device": "laptop",
        "device_id": "7c9e6679-7425-40de-944b-e07fc1f90ae7", "kind": "connector",
        "agent": "claude.ai", "role": "orchestrator",
        "accept": {"from": "users", "users": ["alice", "bob"]}, "rev": rev,
        "field_from_the_future": 1,
    });
    let put = write_ok(&nc, "manifest.put", json!({"record": record})).await;
    assert_eq!(
        put,
        json!({"identity": id, "result": "applied", "rev": rev})
    );

    let got = write_ok(&nc, "manifest.get", json!({"identity": id})).await;
    assert_eq!(got["rev"], rev);
    assert_eq!(got["removed"], false);
    assert_eq!(got["record"]["user"], "alice");
    assert_eq!(got["record"]["kind"], "connector");
    assert_eq!(got["record"]["accept"]["users"][1], "bob");
    assert!(got["record"].get("field_from_the_future").is_none());

    let removed = write_ok(
        &nc,
        "manifest.remove",
        json!({"identity": id, "rev": rev + 1}),
    )
    .await;
    assert_eq!(
        removed,
        json!({"identity": id, "result": "applied", "rev": rev + 1})
    );
    let got = write_ok(&nc, "manifest.get", json!({"identity": id})).await;
    assert_eq!(
        got,
        json!({"identity": id, "rev": rev + 1, "removed": true, "record": null})
    );

    // The tombstone blocks a delayed older put.
    let late = write_ok(
        &nc,
        "manifest.put",
        json!({"record": user_record(&id, "alice", rev)}),
    )
    .await;
    assert_eq!(late["result"], "stale");
    assert_eq!(late["rev"], rev + 1);
}

#[tokio::test]
async fn put_at_or_below_the_stored_rev_is_a_stale_success() {
    let nc = live!();
    let id = unique("stale");
    let rev = next_rev();
    let first = json!({"record": user_record(&id, "alice", rev)});
    assert_eq!(
        write_ok(&nc, "manifest.put", first.clone()).await["result"],
        "applied"
    );

    for (user, at) in [("alice", rev), ("bob", rev), ("bob", rev - 1)] {
        let again = call(
            &nc,
            WRITER,
            "manifest.put",
            json!({"record": user_record(&id, user, at)}),
        )
        .await;
        assert_eq!(again["ok"], true, "stale is a success: {again}");
        assert_eq!(
            again["data"],
            json!({"identity": id, "result": "stale", "rev": rev})
        );
    }
    let got = write_ok(&nc, "manifest.get", json!({"identity": id})).await;
    assert_eq!(got["record"]["user"], "alice", "the first writer wins");

    let older = write_ok(&nc, "manifest.remove", json!({"identity": id, "rev": rev})).await;
    assert_eq!(older["result"], "stale");
}

#[tokio::test]
async fn reserved_and_writer_identities_get_no_record() {
    let nc = live!();
    for id in ["hub-orchestrator", "hub-server", WRITER] {
        let resp = call(
            &nc,
            WRITER,
            "manifest.put",
            json!({"record": user_record(id, "alice", next_rev())}),
        )
        .await;
        assert!(
            error_of(&resp).starts_with("bad manifest record: identity: "),
            "{resp}"
        );
        let resp = call(
            &nc,
            WRITER,
            "manifest.remove",
            json!({"identity": id, "rev": next_rev()}),
        )
        .await;
        assert!(
            error_of(&resp).starts_with("bad manifest record: identity: "),
            "{resp}"
        );
    }
}

#[tokio::test]
async fn bound_non_writer_is_forbidden_from_every_writer_op() {
    let nc = live!();
    let me = unique("nobody");
    let target = unique("target");
    let ops = [
        (
            "manifest.put",
            json!({"record": user_record(&target, "alice", next_rev())}),
        ),
        (
            "manifest.remove",
            json!({"identity": target, "rev": next_rev()}),
        ),
        ("manifest.sync", json!({"rev": next_rev(), "records": []})),
        ("manifest.get", json!({"identity": target})),
        ("manifest.list", json!({})),
    ];
    for (op, params) in ops {
        let resp = call(&nc, &me, op, params).await;
        assert_eq!(error_of(&resp), FORBIDDEN, "{op}");
    }
    let got = write_ok(&nc, "manifest.get", json!({"identity": target})).await;
    assert!(got["rev"].is_null(), "nothing may be written: {got}");
}

/// `hub.api.manifest.put` has no caller identity. It is refused on the
/// permissive default stack too, by the gate rather than by bound mode.
#[tokio::test]
async fn legacy_put_is_refused_in_every_mode() {
    let nc = live!();
    let id = unique("legacy");
    let params = json!({"record": user_record(&id, "alice", next_rev())});
    let resp = raw(
        &nc,
        "hub.api.manifest.put".to_string(),
        "manifest.put",
        params,
    )
    .await;
    let error = error_of(&resp);
    if require_bound() {
        assert!(error.contains("legacy hub.api.<op> rejected"), "{error}");
    } else {
        assert_eq!(error, FORBIDDEN);
    }
    let got = write_ok(&nc, "manifest.get", json!({"identity": id})).await;
    assert!(got["rev"].is_null(), "nothing may be written: {got}");
}

/// `test-admin` is an API admin only in bound mode.
#[tokio::test]
async fn api_admin_may_write_where_it_is_an_admin() {
    let nc = live!();
    let id = unique("byadmin");
    let params = json!({"record": user_record(&id, "alice", next_rev())});
    let resp = call(&nc, "test-admin", "manifest.put", params).await;
    if require_bound() {
        assert_eq!(resp["ok"], true, "{resp}");
        assert_eq!(resp["data"]["result"], "applied");
    } else {
        assert_eq!(error_of(&resp), FORBIDDEN);
    }
}

#[tokio::test]
async fn self_returns_only_the_callers_own_record() {
    let nc = live!();
    let (mine, other) = (unique("mine"), unique("other"));
    for (id, user) in [(&mine, "alice"), (&other, "bob")] {
        write_ok(
            &nc,
            "manifest.put",
            json!({"record": user_record(id, user, next_rev())}),
        )
        .await;
    }

    // Params can't redirect it to someone else's record.
    let resp = call(&nc, &mine, "manifest.self", json!({"identity": other})).await;
    assert_eq!(resp["ok"], true, "{resp}");
    assert_eq!(resp["data"]["identity"], mine.as_str());
    assert_eq!(resp["data"]["record"]["user"], "alice");

    let stranger = unique("stranger");
    let resp = call(&nc, &stranger, "manifest.self", json!({})).await;
    assert_eq!(resp["data"], json!({"identity": stranger, "record": null}));

    // A removed identity has no record either.
    write_ok(
        &nc,
        "manifest.remove",
        json!({"identity": other, "rev": next_rev()}),
    )
    .await;
    let resp = call(&nc, &other, "manifest.self", json!({})).await;
    assert!(resp["data"]["record"].is_null(), "{resp}");
}

#[tokio::test]
async fn bad_records_are_refused_by_field() {
    let nc = live!();
    let id = unique("bad");
    let cases = [
        (
            json!({"identity": id, "owner_kind": "user", "rev": 1}),
            "user: ",
        ),
        (
            json!({"identity": id, "owner_kind": "tenant", "user": "a", "rev": 1}),
            "user: ",
        ),
        (
            json!({"identity": id, "owner_kind": "tenant", "user_id": "not-a-uuid", "rev": 1}),
            "user_id: ",
        ),
        (
            json!({"identity": id, "owner_kind": "tenant", "agent": "a b", "rev": 1}),
            "agent: ",
        ),
        (
            json!({"identity": id, "owner_kind": "tenant", "rev": -1}),
            "rev: ",
        ),
        (
            json!({"identity": "a.b", "owner_kind": "tenant", "rev": 1}),
            "identity: ",
        ),
        (
            json!({"identity": id, "owner_kind": "martian", "rev": 1}),
            "record: ",
        ),
    ];
    for (record, field) in cases {
        let resp = call(&nc, WRITER, "manifest.put", json!({"record": record})).await;
        let want = format!("bad manifest record: {field}");
        assert!(error_of(&resp).starts_with(&want), "{want}: {resp}");
    }
    let resp = call(&nc, WRITER, "manifest.put", json!({})).await;
    assert!(error_of(&resp).starts_with("bad manifest record: record: "));
    for params in [json!({"identity": id}), json!({"rev": 1})] {
        let resp = call(&nc, WRITER, "manifest.remove", params).await;
        assert!(error_of(&resp).starts_with("bad manifest record: "));
    }
    for params in [json!({"records": []}), json!({"rev": 1})] {
        let resp = call(&nc, WRITER, "manifest.sync", params).await;
        assert!(error_of(&resp).starts_with("bad manifest record: "));
    }
    let got = write_ok(&nc, "manifest.get", json!({"identity": id})).await;
    assert!(got["rev"].is_null(), "nothing may be written: {got}");
}

#[tokio::test]
async fn list_pages_through_live_records_in_order() {
    let nc = live!();
    let prefix = unique("page");
    let ids: Vec<String> = (0..5).map(|i| format!("{prefix}-{i}")).collect();
    for id in &ids {
        write_ok(
            &nc,
            "manifest.put",
            json!({"record": user_record(id, "alice", next_rev())}),
        )
        .await;
    }
    write_ok(
        &nc,
        "manifest.remove",
        json!({"identity": ids[4], "rev": next_rev()}),
    )
    .await;

    let mut seen = Vec::new();
    let mut after = Value::Null;
    loop {
        let page = write_ok(&nc, "manifest.list", json!({"limit": 2, "after": after})).await;
        let records = page["records"].as_array().unwrap();
        assert!(records.len() <= 2, "{page}");
        seen.extend(
            records
                .iter()
                .map(|r| r["identity"].as_str().unwrap().to_string()),
        );
        after = page["next_after"].clone();
        if after.is_null() {
            break;
        }
    }
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(seen, sorted, "sorted by identity");
    let mine: Vec<&String> = seen.iter().filter(|id| id.starts_with(&prefix)).collect();
    assert_eq!(
        mine,
        ids[..4].iter().collect::<Vec<_>>(),
        "live records only"
    );

    for params in [
        json!({"limit": 0}),
        json!({"limit": 1001}),
        json!({"after": 3}),
    ] {
        let resp = call(&nc, WRITER, "manifest.list", params).await;
        error_of(&resp);
    }
}

/// Runs below `next_rev`, so it can't tombstone what other tests write.
#[tokio::test]
async fn sync_applies_all_or_nothing_and_tombstones_absent_slots() {
    let nc = live!();
    let (doomed, kept_new, newcomer, tomb) = (
        unique("doomed"),
        unique("after-snapshot"),
        unique("newcomer"),
        unique("tomb"),
    );
    let snapshot = 1_000;
    write_ok(
        &nc,
        "manifest.put",
        json!({"record": user_record(&doomed, "alice", 10)}),
    )
    .await;
    write_ok(
        &nc,
        "manifest.put",
        json!({"record": user_record(&kept_new, "alice", snapshot)}),
    )
    .await;
    write_ok(&nc, "manifest.remove", json!({"identity": tomb, "rev": 20})).await;

    // One bad record, in any position, applies nothing.
    let bad_snapshots = [
        json!({"identity": newcomer, "owner_kind": "user", "rev": 5}),
        user_record("hub-orchestrator", "alice", 5),
        user_record(WRITER, "alice", 5),
        user_record(&newcomer, "alice", snapshot + 1),
    ];
    for bad in bad_snapshots {
        let records = json!([user_record(&unique("ok"), "alice", 5), bad]);
        let resp = call(
            &nc,
            WRITER,
            "manifest.sync",
            json!({"rev": snapshot, "records": records}),
        )
        .await;
        assert!(
            error_of(&resp).starts_with("bad manifest record: "),
            "{resp}"
        );
    }
    let dup = json!([
        user_record(&newcomer, "alice", 5),
        user_record(&newcomer, "bob", 6)
    ]);
    let resp = call(
        &nc,
        WRITER,
        "manifest.sync",
        json!({"rev": snapshot, "records": dup}),
    )
    .await;
    assert!(error_of(&resp).contains("duplicate"), "{resp}");
    let got = write_ok(&nc, "manifest.get", json!({"identity": newcomer})).await;
    assert!(
        got["rev"].is_null(),
        "a refused sync applies nothing: {got}"
    );
    let got = write_ok(&nc, "manifest.get", json!({"identity": doomed})).await;
    assert_eq!(got["removed"], false, "a refused sync removes nothing");

    // A snapshot holding only an old copy of an already-removed identity.
    let report = write_ok(
        &nc,
        "manifest.sync",
        json!({"rev": snapshot, "records": [
            user_record(&newcomer, "alice", 500),
            user_record(&tomb, "alice", 15),
        ]}),
    )
    .await;
    assert_eq!(report["rev"], snapshot);
    assert_eq!(report["applied"], 1, "{report}");
    assert_eq!(report["stale"], 1, "{report}");
    assert!(report["removed"].as_u64().unwrap() >= 1, "{report}");
    assert!(report["live"].as_u64().unwrap() >= 2, "{report}");

    let got = write_ok(&nc, "manifest.get", json!({"identity": doomed})).await;
    assert_eq!(
        got,
        json!({"identity": doomed, "rev": snapshot, "removed": true, "record": null})
    );
    let got = write_ok(&nc, "manifest.get", json!({"identity": kept_new})).await;
    assert_eq!(got["removed"], false, "slots at the snapshot rev are kept");
    let got = write_ok(&nc, "manifest.get", json!({"identity": newcomer})).await;
    assert_eq!(got["record"]["user"], "alice");
    let got = write_ok(&nc, "manifest.get", json!({"identity": tomb})).await;
    assert_eq!(got["removed"], true, "tombstones stay as they are");
}
