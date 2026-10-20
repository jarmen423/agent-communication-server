//! Owner-scoped query-API writes (iteration-2 integration of T1 authz with
//! T2's server-side waves/sessions): in bound-identity mode a non-admin may
//! mutate only the waves/sessions it owns. See `src/query_api/write_authz.rs`.

use std::collections::BTreeSet;
use std::sync::Arc;

use nats_hub::query_api::{handle_request_authorized, ApiAuthz, ApiResponse};
use nats_hub::{SessionRecord, Storage, SurrealStorage, WaveRecord};
use serde_json::{json, Value};

async fn storage() -> Arc<dyn Storage> {
    let s = SurrealStorage::connect_memory().await.unwrap();
    s.migrate().await.unwrap();
    Arc::new(s)
}

fn authz() -> ApiAuthz {
    ApiAuthz {
        require_bound: true,
        admins: BTreeSet::from(["boss".to_string()]),
    }
}

async fn call(s: &Arc<dyn Storage>, caller: &str, op: &str, params: Value) -> ApiResponse {
    let body = serde_json::to_vec(&json!({"op": op, "params": params})).unwrap();
    handle_request_authorized(s, &format!("hub.api.{caller}.{op}"), &body, &authz()).await
}

fn forbidden(r: &ApiResponse) -> bool {
    !r.ok && r.error.as_deref().unwrap_or("").contains("forbidden")
}

const NOW: &str = "2026-09-30T00:00:00Z";

fn wave(id: &str, orchestrator: &str) -> Value {
    json!({
        "wave": {"wave_id": id, "goal": "g", "status": "pending",
                 "orchestrator": orchestrator, "created_at": NOW},
        "tasks": [{"task_id": "t1", "worker": "w1", "goal": "do it"}]
    })
}

fn session(id: &str, orchestrator: &str, worker: &str) -> Value {
    json!({"session_id": id, "orchestrator": orchestrator, "worker": worker,
           "status": "active", "created_at": NOW, "updated_at": NOW})
}

#[tokio::test]
async fn wave_writes_are_owner_scoped() {
    let s = storage().await;

    // Creating a wave for someone else is forbidden; for yourself it's allowed.
    assert!(forbidden(
        &call(&s, "alice", "wave.create", wave("w-a", "bob")).await
    ));
    let r = call(&s, "alice", "wave.create", wave("w-a", "alice")).await;
    assert!(r.ok, "{:?}", r.error);

    // spawn/cancel: the owner gets past authz (the orchestrator isn't running
    // in this unit test, so the handler itself reports that); others are forbidden.
    for op in ["wave.spawn", "wave.cancel", "wave.update_status"] {
        let p = json!({"wave_id": "w-a", "status": "running"});
        assert!(forbidden(&call(&s, "mallory", op, p.clone()).await), "{op}");
        assert!(!forbidden(&call(&s, "alice", op, p).await), "{op}");
    }

    // Task status: the task's worker or the wave owner, nobody else.
    let p = json!({"wave_id": "w-a", "task_id": "t1", "status": "done"});
    assert!(forbidden(
        &call(&s, "mallory", "wave.update_task_status", p.clone()).await
    ));
    assert!(
        call(&s, "w1", "wave.update_task_status", p.clone())
            .await
            .ok
    );
    assert!(call(&s, "alice", "wave.update_task_status", p).await.ok);

    // wave.status is a read: owner/participant sees it, outsiders get "not found".
    let r = call(&s, "mallory", "wave.status", json!({"wave_id": "w-a"})).await;
    assert!(!r.ok && !forbidden(&r));
    assert!(!forbidden(
        &call(&s, "alice", "wave.status", json!({"wave_id": "w-a"})).await
    ));
}

#[tokio::test]
async fn session_writes_are_owner_scoped() {
    let s = storage().await;

    assert!(forbidden(
        &call(&s, "alice", "session.create", session("s1", "bob", "w1")).await
    ));
    let r = call(&s, "alice", "session.create", session("s1", "alice", "w1")).await;
    assert!(r.ok, "{:?}", r.error);

    // The session's worker can persist its backend context (T3 resume hook).
    let ctx = json!({"session_id": "s1", "backend_ctx": {"claude_session_id": "abc"}});
    assert!(forbidden(
        &call(&s, "mallory", "session.set_backend_ctx", ctx.clone()).await
    ));
    let r = call(&s, "w1", "session.set_backend_ctx", ctx).await;
    assert!(r.ok, "{:?}", r.error);

    // Orchestrator or worker may update status; outsiders may not.
    let p = json!({"session_id": "s1", "status": "closed"});
    assert!(forbidden(
        &call(&s, "mallory", "session.update_status", p.clone()).await
    ));
    assert!(call(&s, "w1", "session.update_status", p.clone()).await.ok);
    assert!(call(&s, "alice", "session.update_status", p).await.ok);

    // Admins keep full access.
    let p = json!({"session_id": "s1", "status": "active"});
    assert!(call(&s, "boss", "session.update_status", p).await.ok);
}

// ── Create must not take over an existing id (hardening H1 #2) ─────

#[tokio::test]
async fn create_cannot_take_over_another_owners_wave_or_session() {
    let s = storage().await;
    assert!(
        call(&s, "bob", "wave.create", wave("w-bob", "bob"))
            .await
            .ok
    );
    let r = call(&s, "bob", "session.create", session("s-bob", "bob", "w1")).await;
    assert!(r.ok, "{:?}", r.error);

    // alice re-creates bob's ids naming herself as the orchestrator
    assert!(forbidden(
        &call(&s, "alice", "wave.create", wave("w-bob", "alice")).await
    ));
    let bare = json!({"wave_id": "w-bob", "goal": "mine now", "status": "pending",
                      "orchestrator": "alice", "created_at": NOW});
    assert!(forbidden(&call(&s, "alice", "wave.create", bare).await));
    assert!(forbidden(
        &call(
            &s,
            "alice",
            "session.create",
            session("s-bob", "alice", "w9")
        )
        .await
    ));
    // …or overwrites a task of bob's wave
    let task = json!({"wave_id": "w-bob", "task_id": "t1", "worker": "alice",
                      "goal": "x", "status": "pending", "created_at": NOW});
    assert!(forbidden(
        &call(&s, "alice", "wave.create_task", task).await
    ));

    // nothing changed hands
    let w = s.get_wave("w-bob").await.unwrap().unwrap();
    assert_eq!((w.orchestrator.as_str(), w.goal.as_str()), ("bob", "g"));
    let t = s.get_wave_task("w-bob", "t1").await.unwrap().unwrap();
    assert_eq!(t.worker, "w1");
    let sess = s.get_session("s-bob").await.unwrap().unwrap();
    assert_eq!(
        (sess.orchestrator.as_str(), sess.worker.as_str()),
        ("bob", "w1")
    );

    // the owner may still re-create its own ids (idempotent create)
    assert!(
        call(&s, "bob", "wave.create", wave("w-bob", "bob"))
            .await
            .ok
    );
    let r = call(&s, "bob", "session.create", session("s-bob", "bob", "w2")).await;
    assert!(r.ok, "{:?}", r.error);
}

#[tokio::test]
async fn storage_create_refuses_to_change_the_orchestrator() {
    // Defence in depth for the check-then-write race: the storage upsert
    // itself refuses to re-own an existing record.
    let s = storage().await;
    let mut w: WaveRecord = serde_json::from_value(wave("w-x", "bob")["wave"].clone()).unwrap();
    s.create_wave(w.clone()).await.unwrap();
    w.orchestrator = "alice".into();
    let err = s.create_wave(w.clone()).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("another orchestrator"),
        "{err:#}"
    );
    // same owner: replace is allowed (the orchestrator re-saves its waves)
    w.orchestrator = "bob".into();
    w.status = "running".into();
    s.create_wave(w).await.unwrap();
    assert_eq!(s.get_wave("w-x").await.unwrap().unwrap().status, "running");

    let mut sess: SessionRecord = serde_json::from_value(session("s-x", "bob", "w1")).unwrap();
    s.create_session(sess.clone()).await.unwrap();
    sess.orchestrator = "alice".into();
    let err = s.create_session(sess.clone()).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("another orchestrator"),
        "{err:#}"
    );
    sess.orchestrator = "bob".into();
    sess.backend_ctx = Some(json!({"k": "v"}));
    s.create_session(sess).await.unwrap();
    let got = s.get_session("s-x").await.unwrap().unwrap();
    assert_eq!(got.backend_ctx, Some(json!({"k": "v"})));
}
