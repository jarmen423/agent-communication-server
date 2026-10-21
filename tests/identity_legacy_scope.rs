//! Query-API scoping hardening (H1 #4, #9), no NATS needed:
//!
//! - With `--api-admin` set but without `--require-bound-identity`, a legacy
//!   `hub.api.<op>` caller is an anonymous non-admin that owns nothing.
//! - Scoped `history.query` / `thread.get` over-fetch, so rows the caller
//!   can't see don't shorten its page.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use nats_hub::protocol::{Envelope, MessageKind};
use nats_hub::query_api::{handle_request_authorized, ApiAuthz, ApiResponse};
use nats_hub::{Storage, SurrealStorage};
use serde_json::{json, Value};

async fn storage() -> Arc<dyn Storage> {
    let s = SurrealStorage::connect_memory().await.unwrap();
    s.migrate().await.unwrap();
    Arc::new(s)
}

/// `--api-admin boss`, legacy subjects still accepted.
fn admin_only() -> ApiAuthz {
    ApiAuthz {
        require_bound: false,
        admins: BTreeSet::from(["boss".to_string()]),
        ..Default::default()
    }
}

async fn call(s: &Arc<dyn Storage>, authz: &ApiAuthz, subject: &str, params: Value) -> ApiResponse {
    let op = subject
        .strip_prefix("hub.api.")
        .map(|rest| match rest.split_once('.') {
            Some((first, op))
                if ![
                    "wave", "session", "agent", "history", "thread", "envelope", "stats",
                ]
                .contains(&first) =>
            {
                op
            }
            _ => rest,
        })
        .unwrap();
    let body = serde_json::to_vec(&json!({"op": op, "params": params})).unwrap();
    handle_request_authorized(s, subject, &body, authz).await
}

fn forbidden(r: &ApiResponse) -> bool {
    !r.ok && r.error.as_deref().unwrap_or("").contains("forbidden")
}

fn rows<'a>(r: &'a ApiResponse, key: &str) -> &'a Vec<Value> {
    assert!(r.ok, "{:?}", r.error);
    r.data.as_ref().unwrap()[key].as_array().unwrap()
}

const NOW: &str = "2026-09-30T00:00:00Z";

#[tokio::test]
async fn legacy_caller_owns_nothing_once_api_admin_is_set() {
    let s = storage().await;
    let authz = admin_only();
    let dm = Envelope::new("alice", "chat", MessageKind::Message, json!({"t": "dm"})).to("bob");
    s.store_envelope(&dm).await.unwrap();
    s.store_envelope(&Envelope::new(
        "carol",
        "chat",
        MessageKind::Message,
        json!({"t": "bc"}),
    ))
    .await
    .unwrap();
    let wave = json!({"wave": {"wave_id": "w-bob", "goal": "g", "status": "pending",
                               "orchestrator": "bob", "created_at": NOW},
                      "tasks": [{"task_id": "t1", "worker": "w1", "goal": "do"}]});
    assert!(call(&s, &authz, "hub.api.bob.wave.create", wave).await.ok);
    let sess = json!({"session_id": "s-bob", "orchestrator": "bob", "worker": "w1",
                      "status": "active", "created_at": NOW, "updated_at": NOW});
    assert!(
        call(&s, &authz, "hub.api.bob.session.create", sess)
            .await
            .ok
    );

    // writes: forbidden, including creating a fresh wave/session
    let mine = json!({"wave": {"wave_id": "w-anon", "goal": "g", "status": "pending",
                               "orchestrator": "anon", "created_at": NOW}});
    assert!(forbidden(
        &call(&s, &authz, "hub.api.wave.create", mine).await
    ));
    let p = json!({"session_id": "s-bob", "status": "closed"});
    assert!(forbidden(
        &call(&s, &authz, "hub.api.session.update_status", p).await
    ));
    let p = json!({"wave_id": "w-bob"});
    assert!(forbidden(&call(&s, &authz, "hub.api.wave.cancel", p).await));

    // reads: broadcasts only; no waves, sessions, DMs or pending inboxes
    let r = call(&s, &authz, "hub.api.history.query", json!({})).await;
    let envs = rows(&r, "envelopes");
    assert_eq!(envs.len(), 1, "{envs:?}");
    assert_eq!(envs[0]["from_identity"], "carol");
    let r = call(
        &s,
        &authz,
        "hub.api.envelope.get",
        json!({"id": dm.meta.id}),
    )
    .await;
    assert!(!r.ok, "DM leaked to a legacy caller");
    let r = call(
        &s,
        &authz,
        "hub.api.thread.pending",
        json!({"identity": "bob"}),
    )
    .await;
    assert!(forbidden(&r));
    let r = call(&s, &authz, "hub.api.wave.get", json!({"wave_id": "w-bob"})).await;
    assert!(!r.ok && r.error.as_deref() == Some("wave not found"));
    assert!(rows(
        &call(&s, &authz, "hub.api.wave.list", json!({})).await,
        "waves"
    )
    .is_empty());
    let r = call(
        &s,
        &authz,
        "hub.api.session.get",
        json!({"session_id": "s-bob"}),
    )
    .await;
    assert!(!r.ok);
    assert!(rows(
        &call(&s, &authz, "hub.api.session.list", json!({})).await,
        "sessions"
    )
    .is_empty());
    // unscoped metadata stays available
    assert!(call(&s, &authz, "hub.api.ping", json!({})).await.ok);

    // the same legacy calls on a fully permissive hub are unscoped (compat)
    let open = ApiAuthz::permissive();
    let r = call(&s, &open, "hub.api.history.query", json!({})).await;
    assert_eq!(rows(&r, "envelopes").len(), 2);
    assert!(
        call(&s, &open, "hub.api.wave.get", json!({"wave_id": "w-bob"}))
            .await
            .ok
    );
}

#[tokio::test]
async fn scoped_history_pages_are_not_short() {
    let s = storage().await;
    let authz = admin_only();
    let base = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    // 3 old broadcasts carol can see…
    for i in 0..3 {
        let mut e = Envelope::new("dave", "chat", MessageKind::Message, json!({"bc": i}));
        e.meta.timestamp = base + Duration::seconds(i);
        s.store_envelope(&e).await.unwrap();
    }
    // …buried under more than one MAX_LIMIT page of newer DMs she can't.
    for i in 0..1005 {
        let mut e =
            Envelope::new("alice", "chat", MessageKind::Message, json!({"dm": i})).to("bob");
        e.meta.timestamp = base + Duration::minutes(10) + Duration::milliseconds(i);
        s.store_envelope(&e).await.unwrap();
    }

    let r = call(
        &s,
        &authz,
        "hub.api.carol.history.query",
        json!({"limit": 3}),
    )
    .await;
    let envs = rows(&r, "envelopes");
    assert_eq!(envs.len(), 3, "short page: {envs:?}");
    assert!(envs.iter().all(|e| e["from_identity"] == "dave"));
    assert_eq!(r.data.as_ref().unwrap()["limit"], 3);

    // bob is a party to the DMs: the limit is honoured exactly, newest first
    let r = call(&s, &authz, "hub.api.bob.history.query", json!({"limit": 7})).await;
    let envs = rows(&r, "envelopes");
    assert_eq!(envs.len(), 7);
    assert_eq!(envs[0]["payload"]["dm"], 1004);
}

#[tokio::test]
async fn scoped_thread_pages_are_not_short() {
    let s = storage().await;
    let authz = admin_only();
    let root = Envelope::new("dave", "chat", MessageKind::Message, json!({"root": true}));
    s.store_envelope(&root).await.unwrap();
    for i in 0..10 {
        let e = Envelope::new("alice", "chat", MessageKind::Message, json!({"dm": i}))
            .to("bob")
            .reply_to(root.meta.id.clone());
        s.store_envelope(&e).await.unwrap();
    }
    for i in 0..2 {
        let e = Envelope::new("dave", "chat", MessageKind::Message, json!({"bc": i}))
            .reply_to(root.meta.id.clone());
        s.store_envelope(&e).await.unwrap();
    }
    let r = call(
        &s,
        &authz,
        "hub.api.carol.thread.get",
        json!({"root_id": root.meta.id, "limit": 3}),
    )
    .await;
    let thread = rows(&r, "thread");
    assert_eq!(thread.len(), 3, "{thread:?}");
    assert!(thread.iter().all(|e| e["from_identity"] == "dave"));
}
