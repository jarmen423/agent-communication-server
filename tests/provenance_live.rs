//! `meta.origin` over the wire against `with_stack.sh` (`--tenant test-tenant
//! --manifest-writer test-manifest`): what the router stamps on broadcasts and
//! DMs, what it discards, what history filters on, and what a durable inbox
//! stores. Passes in the default, bound and durable modes.

use std::time::Duration;

use async_nats::jetstream;
use futures_util::StreamExt;
use nats_hub::delivery::{attach_inbox, Attach};
use serde_json::{json, Value};

const TENANT: &str = "test-tenant";

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

async fn api(nc: &async_nats::Client, caller: &str, op: &str, params: Value) -> Value {
    let body = serde_json::to_vec(&json!({"op": op, "params": params})).unwrap();
    let msg = tokio::time::timeout(
        Duration::from_secs(10),
        nc.request(format!("hub.api.{caller}.{op}"), body.into()),
    )
    .await
    .expect("no reply in time")
    .expect("request failed");
    serde_json::from_slice(&msg.payload).unwrap()
}

/// A manifest record for `identity`, owned by `user`.
async fn provision(nc: &async_nats::Client, identity: &str, user: &str) {
    let record = json!({
        "identity": identity, "owner_kind": "user", "user": user,
        "user_id": "6f9619ff-8b86-4011-b42d-00c04fc964ff", "device": "laptop",
        "agent": "claude-code", "rev": chrono::Utc::now().timestamp_millis(),
    });
    let resp = api(
        nc,
        "test-manifest",
        "manifest.put",
        json!({"record": record}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
}

/// An envelope whose `meta.origin` forges every trusted field.
fn forged(channel: &str, to: Option<&str>, model: &str, session: &str) -> Vec<u8> {
    let mut meta = json!({
        "id": uuid::Uuid::new_v4().to_string(), "from": "mallory", "channel": channel,
        "timestamp": "2026-01-01T00:00:00Z", "kind": "message",
        "origin": {
            "tenant": "forged", "identity": "forged", "user": "forged",
            "user_id": "forged", "owner_kind": "tenant", "device": "forged",
            "agent": "forged", "on_behalf_of": {"identity": "forged"},
            "reported": {"model": model, "session": session, "model_provider": "acme"},
        },
    });
    if let Some(to) = to {
        meta["to"] = json!(to);
    }
    serde_json::to_vec(&json!({"meta": meta, "payload": {"t": "hi"}})).unwrap()
}

async fn next_envelope(sub: &mut async_nats::Subscriber) -> Value {
    let msg = tokio::time::timeout(Duration::from_secs(10), sub.next())
        .await
        .expect("no envelope arrived")
        .unwrap();
    serde_json::from_slice(&msg.payload).unwrap()
}

fn assert_stamped(env: &Value, sender: &str, user: &str, model: &str, session: &str) {
    let origin = &env["meta"]["origin"];
    assert_eq!(
        *origin,
        json!({
            "tenant": TENANT,
            "identity": sender,
            "user": user,
            "user_id": "6f9619ff-8b86-4011-b42d-00c04fc964ff",
            "owner_kind": "user",
            "device": "laptop",
            "agent": "claude-code",
            "reported": {"model": model, "session": session, "model_provider": "acme"},
        }),
        "forged fields must be gone and the manifest's fields present"
    );
    assert_eq!(env["meta"]["from"], sender);
}

async fn subscribed(nc: &async_nats::Client, subject: String) -> async_nats::Subscriber {
    let sub = nc.subscribe(subject).await.unwrap();
    nc.flush().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    sub
}

async fn publish(nc: &async_nats::Client, subject: String, body: Vec<u8>) {
    nc.publish(subject, body.into()).await.unwrap();
    nc.flush().await.unwrap();
}

#[tokio::test]
async fn broadcast_carries_manifest_origin_and_history_filters_on_it() {
    let Some(url) = stack_url() else { return };
    let nc = async_nats::connect(&url).await.unwrap();
    let (sender, user) = (unique("prov-a"), unique("alice"));
    let (channel, model, session) = (unique("prov"), unique("model"), unique("session"));
    provision(&nc, &sender, &user).await;

    let mut sub = subscribed(&nc, format!("channel.{channel}")).await;
    publish(
        &nc,
        format!("hub.pub.{sender}.{channel}"),
        forged(&channel, None, &model, &session),
    )
    .await;
    let env = next_envelope(&mut sub).await;
    assert_stamped(&env, &sender, &user, &model, &session);

    // The mirror is asynchronous, so wait for the stored copy.
    let id = env["meta"]["id"].as_str().unwrap();
    let by = [
        ("user", user.as_str()),
        ("agent", "claude-code"),
        ("model", model.as_str()),
        ("session", session.as_str()),
    ];
    for (filter, value) in by {
        let params = json!({"channel": channel, filter: value});
        let data = history_with(&nc, &sender, params, id).await;
        assert_eq!(
            data["applied_filters"],
            json!(["channel", filter]),
            "{data}"
        );
        let stored = &data["envelopes"][0];
        assert_eq!(stored["origin"]["user"], user.as_str(), "{stored}");
        assert_eq!(stored["origin"]["reported"]["model"], model.as_str());
    }

    let params = json!({"channel": channel, "user": "somebody-else", "limit": 5});
    let resp = api(&nc, &sender, "history.query", params).await;
    assert_eq!(resp["data"]["envelopes"], json!([]), "{resp}");
}

/// Poll `history.query` until the envelope `id` shows up.
async fn history_with(nc: &async_nats::Client, caller: &str, params: Value, id: &str) -> Value {
    for _ in 0..50 {
        let resp = api(nc, caller, "history.query", params.clone()).await;
        assert_eq!(resp["ok"], true, "{resp}");
        let found = resp["data"]["envelopes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"].as_str().is_some_and(|got| got.contains(id)));
        if found {
            return resp["data"].clone();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("envelope {id} never matched {params}");
}

#[tokio::test]
async fn dm_carries_manifest_origin() {
    let Some(url) = stack_url() else { return };
    let nc = async_nats::connect(&url).await.unwrap();
    let (sender, rcpt, user) = (unique("prov-a"), unique("prov-x"), unique("alice"));
    let (channel, model, session) = (unique("prov"), unique("model"), unique("session"));
    provision(&nc, &sender, &user).await;

    let mut inbox = subscribed(&nc, format!("channel.inbox.{rcpt}")).await;
    publish(
        &nc,
        format!("hub.pub.{sender}.{channel}"),
        forged(&channel, Some(&rcpt), &model, &session),
    )
    .await;
    let env = next_envelope(&mut inbox).await;
    assert_eq!(env["meta"]["to"], rcpt.as_str());
    assert_stamped(&env, &sender, &user, &model, &session);
}

/// With `--durable` the inbox stream stores what was routed: the stored copy
/// has the same origin a live subscriber saw.
#[tokio::test]
async fn durable_inbox_stores_the_same_origin() {
    let Some(url) = stack_url() else { return };
    if std::env::var("NATS_HUB_DURABLE").is_err() {
        eprintln!("Skipping — needs the durable stack (NATS_HUB_DURABLE=1)");
        return;
    }
    let nc = async_nats::connect(&url).await.unwrap();
    let (sender, rcpt, user) = (unique("prov-a"), unique("prov-x"), unique("alice"));
    let (channel, model, session) = (unique("prov"), unique("model"), unique("session"));
    provision(&nc, &sender, &user).await;

    let target = match attach_inbox(&nc, &rcpt, Duration::from_secs(10)).await {
        Attach::Attached(target) => target,
        other => panic!("attach failed: {other:?}"),
    };
    let mut live = subscribed(&nc, format!("channel.inbox.{rcpt}")).await;
    publish(
        &nc,
        format!("hub.pub.{sender}.{channel}"),
        forged(&channel, Some(&rcpt), &model, &session),
    )
    .await;
    let seen = next_envelope(&mut live).await;
    assert_stamped(&seen, &sender, &user, &model, &session);

    let js = jetstream::new(nc.clone());
    let consumer: jetstream::consumer::PullConsumer = js
        .get_consumer_from_stream(&target.consumer, &target.stream)
        .await
        .unwrap();
    let mut batch = consumer
        .fetch()
        .max_messages(1)
        .expires(Duration::from_secs(10))
        .messages()
        .await
        .unwrap();
    let stored = batch
        .next()
        .await
        .expect("the inbox stream stored nothing")
        .unwrap();
    let stored_env: Value = serde_json::from_slice(&stored.payload).unwrap();
    stored.ack().await.unwrap();
    assert_eq!(stored_env["meta"]["id"], seen["meta"]["id"]);
    assert_eq!(stored_env["meta"]["origin"], seen["meta"]["origin"]);
}

/// An identity with no manifest record gets the hub's tenant and its bound
/// identity, and its forged fields are still discarded.
#[tokio::test]
async fn bound_sender_without_a_record_gets_tenant_and_identity_only() {
    let Some(url) = stack_url() else { return };
    let nc = async_nats::connect(&url).await.unwrap();
    let (sender, channel, model) = (unique("prov-none"), unique("prov"), unique("model"));

    let mut sub = subscribed(&nc, format!("channel.{channel}")).await;
    publish(
        &nc,
        format!("hub.pub.{sender}.{channel}"),
        forged(&channel, None, &model, "s-1"),
    )
    .await;
    let env = next_envelope(&mut sub).await;
    assert_eq!(
        env["meta"]["origin"],
        json!({
            "tenant": TENANT,
            "identity": sender,
            "reported": {"model": model, "session": "s-1", "model_provider": "acme"},
        })
    );
}

/// A legacy subject proves nothing about the sender, so a `meta.from` naming a
/// manifest identity must not pick up that identity's record. Bound mode drops
/// the send instead.
#[tokio::test]
async fn legacy_send_is_never_looked_up_in_the_manifest() {
    let Some(url) = stack_url() else { return };
    let nc = async_nats::connect(&url).await.unwrap();
    let (named, channel, model) = (unique("prov-a"), unique("prov"), unique("model"));
    provision(&nc, &named, "alice").await;

    let mut sub = subscribed(&nc, format!("channel.{channel}")).await;
    let mut body: Value = serde_json::from_slice(&forged(&channel, None, &model, "s-1")).unwrap();
    body["meta"]["from"] = json!(named);
    let body = serde_json::to_vec(&body).unwrap();
    publish(&nc, format!("hub.send.{channel}"), body).await;

    if std::env::var("NATS_HUB_REQUIRE_BOUND").is_ok() {
        let got = tokio::time::timeout(Duration::from_secs(1), sub.next()).await;
        assert!(got.is_err(), "bound mode must drop a legacy send: {got:?}");
        return;
    }
    let env = next_envelope(&mut sub).await;
    assert_eq!(
        env["meta"]["origin"],
        json!({
            "tenant": TENANT,
            "reported": {"model": model, "session": "s-1", "model_provider": "acme"},
        })
    );
}
