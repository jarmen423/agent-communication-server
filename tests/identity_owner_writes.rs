//! Owner-scoped query-API writes (iteration-2 integration of T1 authz with
//! T2's server-side waves/sessions): in bound-identity mode a non-admin may
//! mutate only the waves/sessions it owns. See `src/query_api/write_authz.rs`.

use std::collections::BTreeSet;
use std::sync::Arc;

use nats_hub::query_api::{handle_request_authorized, ApiAuthz, ApiResponse};
use nats_hub::{Storage, SurrealStorage};
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
