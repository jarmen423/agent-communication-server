//! File locks (W2-D): conflict rules, all-or-nothing acquire, concurrent
//! single-winner, TTL and presence expiry, restart persistence, holder
//! authz — plus live tests over a real nats-server + hub-server.
#![allow(clippy::await_holding_lock)] // STACK_LOCK serializes live tests on purpose

use std::sync::Arc;
use std::time::Duration;

use nats_hub::query_api::{handle_request_authorized, ApiAuthz, Caller};
use nats_hub::{ApiClient, Envelope, HubClient, MessageKind, Storage, SurrealStorage};
use serde_json::{json, Value};

#[path = "wave_support/mod.rs"]
mod wave_support;
use wave_support::*;

// ── In-process service over memory storage ────────────────────

async fn storage() -> Arc<dyn Storage> {
    let s = SurrealStorage::connect_memory().await.unwrap();
    s.migrate().await.unwrap();
    Arc::new(s)
}

fn service(s: &Arc<dyn Storage>) -> Arc<nats_hub::locks::LockService> {
    nats_hub::locks::LockService::with_config(
        s.clone(),
        nats_hub::locks::LockConfig {
            liveness_ttl: Duration::from_secs(3),
            require_bound_identity: false,
        },
    )
}

fn bound(id: &str) -> Caller {
    Caller::Bound(id.to_string())
}

async fn acquire(
    svc: &nats_hub::locks::LockService,
    caller: &str,
    scope: &str,
    paths: &[&str],
) -> nats_hub::ApiResponse {
    svc.handle_write(
        &bound(caller),
        false,
        "lock.acquire",
        &json!({"scope": scope, "paths": paths}),
    )
    .await
}

#[tokio::test]
async fn acquire_conflicts_and_never_partially_grants() {
    let s = storage().await;
    let svc = service(&s);

    let a = acquire(&svc, "alice", "repo", &["src/foo.rs", "docs/"]).await;
    assert_eq!(a.data.as_ref().unwrap()["granted"], true);

    // Overlap on one path of many → refused entirely, conflicts reported.
    let b = acquire(&svc, "bob", "repo", &["docs/guide.md", "other.rs"]).await;
    assert_eq!(b.data.as_ref().unwrap()["granted"], false);
    let conflicts = b.data.as_ref().unwrap()["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0]["holder"], "alice");
    // "other.rs" was NOT granted: bob holds nothing.
    assert!(s
        .list_locks(Some("repo"))
        .await
        .unwrap()
        .iter()
        .all(|l| l.holder != "bob"));

    // Same holder may stack overlapping locks.
    let a2 = acquire(&svc, "alice", "repo", &["src/foo.rs"]).await;
    assert_eq!(a2.data.as_ref().unwrap()["granted"], true);

    // Different scope never conflicts.
    let c = acquire(&svc, "carol", "other-repo", &["src/foo.rs"]).await;
    assert_eq!(c.data.as_ref().unwrap()["granted"], true);
}

#[tokio::test]
async fn concurrent_acquires_exactly_one_wins() {
    let s = storage().await;
    let svc = service(&s);
    let mut tasks = Vec::new();
    for i in 0..16 {
        let svc = svc.clone();
        tasks.push(tokio::spawn(async move {
            acquire(&svc, &format!("agent-{i}"), "repo", &["src/shared.rs"]).await
        }));
    }
    let mut granted = 0;
    for t in tasks {
        if t.await.unwrap().data.unwrap()["granted"] == json!(true) {
            granted += 1;
        }
    }
    assert_eq!(granted, 1, "exactly one acquire may win");
    assert_eq!(s.list_locks(Some("repo")).await.unwrap().len(), 1);
}

#[tokio::test]
async fn renew_and_release_are_holder_only() {
    let s = storage().await;
    let svc = service(&s);
    let r = acquire(&svc, "alice", "repo", &["x.rs"]).await;
    let lock_id = r.data.unwrap()["lock"]["lock_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Non-holder renew/release denied (caller identity ≠ holder).
    for op in ["lock.renew", "lock.release"] {
        let resp = svc
            .handle_write(&bound("bob"), false, op, &json!({"lock_id": lock_id}))
            .await;
        assert!(!resp.ok, "{op} by non-holder must fail");
        assert!(
            resp.error.as_ref().unwrap().contains("held by 'alice'"),
            "{resp:?}"
        );
    }
    // Admin may renew/release (force).
    let admin = svc
        .handle_write(
            &bound("admin-1"),
            true,
            "lock.release",
            &json!({"lock_id": lock_id}),
        )
        .await;
    assert!(admin.ok, "{:?}", admin.error);
    assert!(s.get_lock(&lock_id).await.unwrap().is_none());
}

#[tokio::test]
async fn holder_mismatch_param_rejected_and_legacy_needs_holder() {
    let s = storage().await;
    let svc = service(&s);
    // Bound caller asserting someone else's holder → error.
    let resp = svc
        .handle_write(
            &bound("alice"),
            false,
            "lock.acquire",
            &json!({"scope": "r", "paths": ["x"], "holder": "mallory"}),
        )
        .await;
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("does not match"));
    // Legacy caller (permissive hub) must name holder.
    let resp = svc
        .handle_write(
            &Caller::Legacy,
            false,
            "lock.acquire",
            &json!({"scope": "r", "paths": ["x"]}),
        )
        .await;
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("missing holder"));
    let resp = svc
        .handle_write(
            &Caller::Legacy,
            false,
            "lock.acquire",
            &json!({"scope": "r", "paths": ["x"], "holder": "legacy-agent"}),
        )
        .await;
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.data.unwrap()["lock"]["holder"], "legacy-agent");
}

#[tokio::test]
async fn ttl_and_presence_expiry() {
    let s = storage().await;
    let svc = service(&s);

    // TTL expiry: a 1s lock swept after its deadline.
    let r = svc
        .handle_write(
            &bound("short"),
            false,
            "lock.acquire",
            &json!({"scope": "r", "paths": ["a"], "ttl_secs": 1}),
        )
        .await;
    assert_eq!(r.data.unwrap()["granted"], true);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    nats_hub::locks::sweep_once(&svc).await;
    assert!(s.list_locks(Some("r")).await.unwrap().is_empty());

    // Presence expiry: holder seen on hub.presence, then silent > ttl.
    let r = acquire(&svc, "ghost", "r", &["b"]).await;
    assert_eq!(r.data.unwrap()["granted"], true);
    let hb = Envelope::new(
        "ghost",
        "system",
        MessageKind::Control,
        json!({"identity": "ghost"}),
    );
    nats_hub::locks::note_presence(&svc, "hub.presence.ghost", &hb).await;
    nats_hub::locks::sweep_once(&svc).await;
    assert_eq!(
        s.list_locks(Some("r")).await.unwrap().len(),
        1,
        "fresh presence keeps lock"
    );
    tokio::time::sleep(Duration::from_secs(4)).await; // liveness_ttl = 3s
    nats_hub::locks::sweep_once(&svc).await;
    assert!(
        s.list_locks(Some("r")).await.unwrap().is_empty(),
        "presence lost → expired"
    );

    // Never-seen holder: TTL only — survives the sweep while unexpired.
    let r = svc
        .handle_write(
            &bound("quiet"),
            false,
            "lock.acquire",
            &json!({"scope": "r", "paths": ["c"], "ttl_secs": 60}),
        )
        .await;
    assert_eq!(r.data.unwrap()["granted"], true);
    nats_hub::locks::sweep_once(&svc).await;
    assert_eq!(s.list_locks(Some("r")).await.unwrap().len(), 1);
}

#[tokio::test]
async fn authz_gate_denies_non_holder_and_anonymous() {
    let s = storage().await;
    let svc = service(&s);
    nats_hub::locks::install(svc.clone());
    let authz = ApiAuthz {
        require_bound: false,
        admins: ["admin-1".into()].into_iter().collect(),
        backup_dir: None,
    };
    let call = |subject: &str, op: &str, params: Value| {
        let body = serde_json::to_vec(&json!({"op": op, "params": params})).unwrap();
        let s = s.clone();
        let a = authz.clone();
        let subj = subject.to_string();
        async move { handle_request_authorized(&s, &subj, &body, &a).await }
    };
    // acquire as bound alice
    let r = call(
        "hub.api.alice.lock.acquire",
        "lock.acquire",
        json!({"scope": "r", "paths": ["x"]}),
    )
    .await;
    assert!(r.ok, "{:?}", r.error);
    let lock_id = r.data.unwrap()["lock"]["lock_id"]
        .as_str()
        .unwrap()
        .to_string();

    // bob can't release alice's lock; anonymous legacy can't write at all.
    let r = call(
        "hub.api.bob.lock.release",
        "lock.release",
        json!({"lock_id": lock_id}),
    )
    .await;
    assert!(
        !r.ok && r.error.as_ref().unwrap().contains("forbidden"),
        "{r:?}"
    );
    let r = call(
        "hub.api.lock.release",
        "lock.release",
        json!({"lock_id": lock_id, "holder": "alice"}),
    )
    .await;
    assert!(
        !r.ok && r.error.as_ref().unwrap().contains("forbidden"),
        "{r:?}"
    );
    // admin can (force).
    let r = call(
        "hub.api.admin-1.lock.release",
        "lock.release",
        json!({"lock_id": lock_id}),
    )
    .await;
    assert!(r.ok, "{:?}", r.error);
    nats_hub::locks::uninstall();
}

// ── Live: real nats-server + hub-server ───────────────────────

fn stack_test() -> bool {
    std::env::var("NATS_URL").is_ok() || nats_server_bin().is_some()
}

#[tokio::test]
async fn live_lock_wait_release_and_events() {
    if !stack_test() {
        eprintln!("no nats-server; skipping live test");
        return;
    }
    let _g = STACK_LOCK.lock().unwrap();
    let Some(stack) = TestStack::new().await else {
        return;
    };
    let _hub = stack.start_hub();
    stack.wait_hub().await;

    // Agent A locks; agent B waits; events land on channel.lock.<hash>.
    let api_a = ApiClient::connect_with_identity(&stack.nats_url, "agent-a")
        .await
        .unwrap();
    let hub_b = HubClient::connect(&stack.nats_url, "agent-b")
        .await
        .unwrap();
    let api_b = ApiClient::connect_with_identity(&stack.nats_url, "agent-b")
        .await
        .unwrap();

    let out = api_a
        .lock_acquire("repo", &["src/main.rs".into()], Default::default())
        .await
        .unwrap();
    assert!(out.granted);
    let lock_id = out.lock.unwrap().lock_id;
    assert_eq!(out.channel, nats_hub_client::locks::lock_channel("repo"));

    let mut events = hub_b.subscribe_channel(&out.channel).await.unwrap();
    let waiter = tokio::spawn({
        let paths = vec!["src/main.rs".to_string()];
        let scope = "repo".to_string();
        async move {
            nats_hub_client::locks::wait_for_locks(
                &hub_b,
                &api_b,
                &scope,
                &paths,
                Duration::from_secs(15),
                Duration::from_millis(500),
            )
            .await
            .unwrap()
        }
    });
    tokio::time::sleep(Duration::from_millis(400)).await;
    api_a.lock_release(&lock_id, false).await.unwrap();

    match tokio::time::timeout(Duration::from_secs(15), waiter)
        .await
        .unwrap()
        .unwrap()
    {
        nats_hub_client::locks::WaitOutcome::Free => {}
        other => panic!("waiter did not unblock: {other:?}"),
    }
    // The release event was broadcast on the lock channel.
    let env = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("no lock event")
        .expect("event stream closed");
    let (ty, _) = nats_hub_client::locks::lock_event(&env).expect("not a lock event");
    assert_eq!(ty, "lock.released");
    assert_eq!(env.meta.from, "hub-locks");
}

#[tokio::test]
async fn live_hub_restart_keeps_locks() {
    if !stack_test() {
        eprintln!("no nats-server; skipping live test");
        return;
    }
    let _g = STACK_LOCK.lock().unwrap();
    let Some(stack) = TestStack::new().await else {
        return;
    };
    let mut hub = stack.start_hub();
    stack.wait_hub().await;
    let api = ApiClient::connect_with_identity(&stack.nats_url, "agent-a")
        .await
        .unwrap();
    let out = api
        .lock_acquire("repo", &["x.rs".into()], Default::default())
        .await
        .unwrap();
    assert!(out.granted);
    let lock_id = out.lock.unwrap().lock_id;

    hub.kill().await.unwrap();
    let _hub2 = stack.start_hub();
    stack.wait_hub().await;

    let lock = api.lock_get(&lock_id).await.unwrap();
    assert!(lock.is_some(), "lock must survive the hub-server restart");
    api.lock_release(&lock_id, false).await.unwrap();
}
