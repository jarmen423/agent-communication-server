//! Identity binding (iteration-2 T1): subject builders + parsing, identity
//! validation, query-API authorization, and live-stack bound-mode checks.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use nats_hub::protocol::{require_valid_identity, subjects, valid_identity, Envelope, MessageKind};
use nats_hub::query_api::{
    handle_request_authorized, subject_bound, ApiAuthz, ApiResponse, Caller,
};
use nats_hub::router::{bound_identity_subject, bound_send_subject};
use nats_hub::{ApiClient, HubClient, Storage, SurrealStorage};
use serde_json::{json, Value};

// ── Subject builders ───────────────────────────────────────────────

#[test]
fn bound_subject_builders_match_contract() {
    assert_eq!(subjects::send_bound("alice", "chat"), "hub.pub.alice.chat");
    assert_eq!(
        subjects::send_bound("alice", "task.1"),
        "hub.pub.alice.task.1"
    );
    assert_eq!(subjects::register_bound("alice"), "hub.register.alice");
    assert_eq!(subjects::presence_bound("alice"), "hub.presence.alice");
    assert_eq!(
        subject_bound("alice", "agent.find"),
        "hub.api.alice.agent.find"
    );
}

#[test]
fn identity_validation_rules() {
    for ok in ["alice", "a-b_c9", "Z", "worker-1_prod_x"] {
        assert!(valid_identity(ok), "{ok} should be valid");
        assert!(require_valid_identity(ok).is_ok());
    }
    for bad in ["", "a.b", "a b", "a*b", "a>b", "a.b.c", "wave.x"] {
        assert!(!valid_identity(bad), "{bad:?} should be invalid");
        assert!(require_valid_identity(bad).is_err());
    }
}

#[test]
fn bound_subject_parsing() {
    assert_eq!(
        bound_send_subject("hub.pub.alice.chat"),
        Some(("alice", "chat"))
    );
    assert_eq!(
        bound_send_subject("hub.pub.alice.task.9"),
        Some(("alice", "task.9"))
    );
    // identity token present but channel missing / malformed
    assert_eq!(bound_send_subject("hub.pub.alice"), None);
    assert_eq!(bound_send_subject("hub.pub."), None);
    assert_eq!(bound_send_subject("hub.send.chat"), None);
    assert_eq!(bound_send_subject("channel.chat"), None);

    assert_eq!(
        bound_identity_subject("hub.register.alice", "hub.register"),
        Some("alice")
    );
    assert_eq!(
        bound_identity_subject("hub.presence.w-1", "hub.presence"),
        Some("w-1")
    );
    // wrong prefix, bare subject, multi-token tail
    assert_eq!(bound_identity_subject("hub.register", "hub.register"), None);
    assert_eq!(
        bound_identity_subject("hub.register.a.b", "hub.register"),
        None
    );
    assert_eq!(
        bound_identity_subject("hub.presence.bob", "hub.register"),
        None
    );
}

// ── Query API subject parsing + authz ──────────────────────────────

#[test]
fn api_subject_parsing() {
    let authz = ApiAuthz::permissive();

    // legacy forms (op namespace first token, or single token)
    let (c, op) = authz.parse_subject("hub.api.wave.get").unwrap();
    assert_eq!(c, Caller::Legacy);
    assert_eq!(op, "wave.get");
    let (c, op) = authz.parse_subject("hub.api.ping").unwrap();
    assert_eq!(c, Caller::Legacy);
    assert_eq!(op, "ping");
    // single unknown token still goes down the legacy unknown-op path
    let (c, op) = authz.parse_subject("hub.api.bogus").unwrap();
    assert_eq!(c, Caller::Legacy);
    assert_eq!(op, "bogus");

    // bound forms
    let (c, op) = authz.parse_subject("hub.api.alice.wave.get").unwrap();
    assert_eq!(c, Caller::Bound("alice".into()));
    assert_eq!(op, "wave.get");

    // multi-token op: identity 'a', op 'b.c'
    let (c, op) = authz.parse_subject("hub.api.a.b.c").unwrap();
    assert_eq!(c, Caller::Bound("a".into()));
    assert_eq!(op, "b.c");

    // require_bound rejects legacy
    let strict = ApiAuthz {
        require_bound: true,
        ..Default::default()
    };
    let resp = strict.parse_subject("hub.api.wave.get");
    assert!(resp.is_err());
    let (c, _) = strict.parse_subject("hub.api.alice.wave.get").unwrap();
    assert_eq!(c, Caller::Bound("alice".into()));
}

#[test]
fn write_op_classification() {
    for w in [
        "wave.create",
        "wave.create_task",
        "wave.update_status",
        "wave.update_task_status",
        "session.create",
        "session.update_status",
        "session.some_future_op",
    ] {
        assert!(ApiAuthz::is_write_op(w), "{w} should be a write op");
    }
    for r in [
        "wave.get",
        "wave.list",
        "wave.list_tasks",
        "wave.get_task",
        "session.get",
        "session.list",
        "history.query",
        "agent.find",
        "ping",
    ] {
        assert!(!ApiAuthz::is_write_op(r), "{r} should be a read op");
    }
}

// ── Authz over a real (in-memory) storage ──────────────────────────

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

async fn call(
    s: &Arc<dyn Storage>,
    authz: &ApiAuthz,
    subject: &str,
    op: &str,
    params: Value,
) -> ApiResponse {
    let body = serde_json::to_vec(&json!({"op": op, "params": params})).unwrap();
    handle_request_authorized(s, subject, &body, authz).await
}

async fn seed(s: &Arc<dyn Storage>) {
    // alice → bob DM
    s.store_envelope(
        &Envelope::new("alice", "chat", MessageKind::Message, json!({"t": "a2b"})).to("bob"),
    )
    .await
    .unwrap();
    // bob → alice DM
    s.store_envelope(
        &Envelope::new("bob", "chat", MessageKind::Message, json!({"t": "b2a"})).to("alice"),
    )
    .await
    .unwrap();
    // broadcast on channel.chat (public)
    s.store_envelope(&Envelope::new(
        "carol",
        "chat",
        MessageKind::Message,
        json!({"t": "bc"}),
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn bound_caller_sees_only_own_dms() {
    let s = storage().await;
    seed(&s).await;
    let authz = authz();

    // bob sees: a→b DM (to=bob) + b→a DM (from=bob) + broadcast = 3
    let resp = call(
        &s,
        &authz,
        "hub.api.bob.history.query",
        "history.query",
        json!({}),
    )
    .await;
    assert!(resp.ok);
    let envs = resp.data.as_ref().unwrap()["envelopes"].as_array().unwrap();
    assert_eq!(envs.len(), 3);
    for e in envs {
        let to = e["to_identity"].as_str().unwrap_or("");
        let from = e["from_identity"].as_str().unwrap_or("");
        assert!(
            to.is_empty() || to == "bob" || from == "bob",
            "leaked env: {e}"
        );
    }

    // carol sees only the broadcast — no DM she isn't a party to
    let resp = call(
        &s,
        &authz,
        "hub.api.carol.history.query",
        "history.query",
        json!({}),
    )
    .await;
    assert!(resp.ok);
    let envs = resp.data.as_ref().unwrap()["envelopes"].as_array().unwrap();
    assert_eq!(envs.len(), 1, "carol must not see alice/bob DMs: {envs:?}");
    assert_eq!(envs[0]["from_identity"], "carol");
}

#[tokio::test]
async fn thread_pending_only_own_identity() {
    let s = storage().await;
    seed(&s).await;
    let authz = authz();

    let resp = call(
        &s,
        &authz,
        "hub.api.bob.thread.pending",
        "thread.pending",
        json!({"identity": "bob"}),
    )
    .await;
    assert!(resp.ok);
    let pending = resp.data.as_ref().unwrap()["pending"].as_array().unwrap();
    assert_eq!(pending.len(), 1);

    let resp = call(
        &s,
        &authz,
        "hub.api.bob.thread.pending",
        "thread.pending",
        json!({"identity": "alice"}),
    )
    .await;
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("forbidden"));
}

#[tokio::test]
async fn envelope_get_hides_invisible_records() {
    let s = storage().await;
    let env = Envelope::new("alice", "chat", MessageKind::Message, json!({})).to("bob");
    let id = env.meta.id.clone();
    s.store_envelope(&env).await.unwrap();
    let authz = authz();

    // carol (neither party) sees "not found", not the envelope
    let resp = call(
        &s,
        &authz,
        "hub.api.carol.envelope.get",
        "envelope.get",
        json!({"id": id}),
    )
    .await;
    assert!(!resp.ok);
    assert_eq!(resp.error.as_deref(), Some("envelope not found"));

    // a party to the DM can fetch it
    let resp = call(
        &s,
        &authz,
        "hub.api.bob.envelope.get",
        "envelope.get",
        json!({"id": id}),
    )
    .await;
    assert!(resp.ok);
}

#[tokio::test]
async fn write_ops_need_admin() {
    let s = storage().await;
    let authz = authz();
    let params = json!({"session_id": "nope", "status": "x"});

    let resp = call(
        &s,
        &authz,
        "hub.api.carol.session.update_status",
        "session.update_status",
        params.clone(),
    )
    .await;
    assert!(!resp.ok);
    assert!(
        resp.error.as_deref().unwrap_or("").contains("forbidden"),
        "{:?}",
        resp.error
    );

    // admin is dispatched — the handler runs (and fails honestly on the
    // missing session), i.e. the response is NOT the forbidden error.
    let resp = call(
        &s,
        &authz,
        "hub.api.boss.session.update_status",
        "session.update_status",
        params,
    )
    .await;
    let err = resp.error.unwrap_or_default();
    assert!(!err.contains("forbidden"), "{err}");
}

#[tokio::test]
async fn admin_bypasses_read_scoping() {
    let s = storage().await;
    seed(&s).await;
    let authz = authz();

    let resp = call(
        &s,
        &authz,
        "hub.api.boss.thread.pending",
        "thread.pending",
        json!({"identity": "alice"}),
    )
    .await;
    assert!(resp.ok);
}

#[tokio::test]
async fn legacy_subject_rejected_in_bound_mode() {
    let s = storage().await;
    let authz = authz();
    let resp = call(&s, &authz, "hub.api.agent.find", "agent.find", json!({})).await;
    assert!(!resp.ok);
    assert!(
        resp.error.unwrap().contains("bound"),
        "expected bound-mode rejection"
    );
}

#[tokio::test]
async fn permissive_mode_is_backward_compatible() {
    let s = storage().await;
    seed(&s).await;
    let permissive = ApiAuthz::permissive();

    // legacy subject → unscoped
    let resp = call(
        &s,
        &permissive,
        "hub.api.history.query",
        "history.query",
        json!({}),
    )
    .await;
    assert!(resp.ok);
    assert_eq!(
        resp.data.as_ref().unwrap()["envelopes"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    // bound subjects also work in permissive mode (scoped to the caller)
    let resp = call(
        &s,
        &permissive,
        "hub.api.bob.history.query",
        "history.query",
        json!({}),
    )
    .await;
    assert!(resp.ok);
    assert_eq!(
        resp.data.as_ref().unwrap()["envelopes"]
            .as_array()
            .unwrap()
            .len(),
        3 // a→b (to=bob) + b→a (from=bob) + broadcast
    );
}

// ── Live-stack checks (with_stack.sh; NATS_HUB_TEST_STACK=1) ───────

fn stack_url() -> Option<String> {
    if std::env::var("NATS_HUB_TEST_STACK").ok().as_deref() != Some("1") {
        eprintln!("Skipping — needs scripts/dev/with_stack.sh (NATS_HUB_TEST_STACK=1)");
        return None;
    }
    Some(std::env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into()))
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

/// A forged envelope on `hub.pub.<claimed>.<channel>` arrives at the
/// recipient's inbox with meta.from rewritten to the subject's identity.
/// True in every mode — the router always overwrites bound-subject `from`.
#[tokio::test]
async fn forged_from_is_rewritten_over_the_wire() {
    let Some(url) = stack_url() else { return };
    let sender_id = unique("alice");
    let rcpt = unique("bob");
    let channel = unique("idn");

    // raw nats clients — no HubClient convenience, we forge the JSON by hand
    let sender = async_nats::connect(&url).await.unwrap();
    let watcher = async_nats::connect(&url).await.unwrap();
    let mut inbox = watcher
        .subscribe(format!("channel.inbox.{rcpt}"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let forged = json!({
        "meta": {"id": uuid::Uuid::new_v4().to_string(), "from": "mallory",
                 "channel": channel, "to": rcpt,
                 "timestamp": "2026-01-01T00:00:00Z", "kind": "message"},
        "payload": {"t": "forged"},
    });
    sender
        .publish(
            format!("hub.pub.{sender_id}.{channel}"),
            forged.to_string().into(),
        )
        .await
        .unwrap();
    sender.flush().await.unwrap();

    let msg = tokio::time::timeout(Duration::from_secs(5), inbox.next())
        .await
        .expect("no envelope arrived")
        .unwrap();
    let env: Value = serde_json::from_slice(&msg.payload).unwrap();
    assert_eq!(env["meta"]["from"], sender_id);
    assert_eq!(env["payload"]["t"], "forged");
}

/// Under `NATS_HUB_REQUIRE_BOUND=1` (with_stack bound switch) the router
/// drops legacy `hub.send.*` while the same message on `hub.pub.<id>.*`
/// routes. In permissive mode both route — the test only asserts the
/// bound subject works and notes the metric difference.
#[tokio::test]
async fn legacy_send_behaves_per_mode() {
    let Some(url) = stack_url() else { return };
    let require_bound = std::env::var("NATS_HUB_REQUIRE_BOUND").is_ok();
    let rcpt = unique("bob");
    let channel = unique("idn");

    let watcher = async_nats::connect(&url).await.unwrap();
    let mut inbox = watcher
        .subscribe(format!("channel.inbox.{rcpt}"))
        .await
        .unwrap();
    let sender = async_nats::connect(&url).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mk = |ch: &str| {
        json!({"meta": {"id": uuid::Uuid::new_v4().to_string(), "from": "alice",
                       "channel": ch, "to": rcpt,
                       "timestamp": "2026-01-01T00:00:00Z", "kind": "message"},
               "payload": {}})
        .to_string()
    };

    // legacy subject
    sender
        .publish(format!("hub.send.{channel}"), mk(&channel).into())
        .await
        .unwrap();
    sender.flush().await.unwrap();
    let legacy = tokio::time::timeout(Duration::from_secs(2), inbox.next()).await;
    if require_bound {
        assert!(
            legacy.is_err(),
            "legacy hub.send routed under require-bound"
        );
    } else {
        assert!(legacy.is_ok(), "legacy hub.send dropped in permissive mode");
    }

    // bound subject always routes
    let alice = unique("alice");
    sender
        .publish(format!("hub.pub.{alice}.{channel}"), mk(&channel).into())
        .await
        .unwrap();
    sender.flush().await.unwrap();
    let bound = tokio::time::timeout(Duration::from_secs(5), inbox.next()).await;
    assert!(bound.is_ok(), "bound hub.pub did not route");
}

/// Register on the bound subject lands in the registry; ApiClient picks
/// the bound API subject automatically.
#[tokio::test]
async fn bound_register_and_api_round_trip() {
    let Some(url) = stack_url() else { return };
    let me = unique("agent");
    let client = HubClient::connect(&url, &me).await.unwrap();
    client.register(vec!["test".into()]).await.unwrap();

    // ApiClient uses NATS_HUB_IDENTITY (set to test-admin by the bound-mode
    // stack switch) or falls back to legacy in permissive mode — both reach
    // the API and agent.find is unscoped.
    let api = ApiClient::connect(&url).await.unwrap();
    let mut found = false;
    for _ in 0..50 {
        let v = api
            .request("agent.find", json!({"capabilities": ["test"]}))
            .await
            .unwrap_or(Value::Null);
        if v["agents"]
            .as_array()
            .map(|a| a.iter().any(|r| r["identity"] == me))
            .unwrap_or(false)
        {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "bound registration never visible via api");
    let _ = client.drain().await;
}
