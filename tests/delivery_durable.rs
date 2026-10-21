//! Durable delivery (W2-A, `hub-server --durable`), acceptance criteria 1–6.
//!
//! Every test boots a PRIVATE JetStream nats-server and its own
//! hub-server(s), so it runs the same under `make test`, `make test-bound`
//! and `make test-durable`. Skipped when no nats-server binary is found.

mod support;

mod delivery_support;

use std::time::Duration;

use delivery_support::*;
use nats_hub::client::{task_channel_for, TaskOutcome};
use nats_hub::{Envelope, HubClient, MessageKind};
use serde_json::json;

macro_rules! stack {
    () => {
        match DurableStack::new().await {
            Some(s) => s,
            None => {
                eprintln!("skipping: nats-server binary not found");
                return;
            }
        }
    };
}

async fn admin_js(url: &str) -> async_nats::jetstream::Context {
    async_nats::jetstream::new(async_nats::connect(url).await.unwrap())
}

/// Messages stored for `subject` in `stream`.
async fn stored_count(url: &str, stream: &str, subject: &str) -> u64 {
    let js = admin_js(url).await;
    let s = js.get_stream(stream).await.unwrap();
    let info = s.info_with_subjects(subject).await.unwrap();
    let mut n = 0;
    futures_util::pin_mut!(info);
    use futures_util::StreamExt;
    while let Some(Ok((_, c))) = info.next().await {
        n += c as u64;
    }
    n
}

// AC1 — a send while hub-server is down is routed once it is back.
#[tokio::test]
async fn send_during_router_outage_is_routed_after_restart() {
    let _g = STACK_LOCK.lock().await;
    let stack = stack!();
    let hub = stack.durable_hub().await;

    let bob_id = unique("bob");
    let bob = HubClient::connect(&stack.url, bob_id.as_str())
        .await
        .unwrap();
    let mut bob_inbox = bob.subscribe_inbox().await.unwrap();
    let alice = HubClient::connect(&stack.url, unique("alice").as_str())
        .await
        .unwrap();

    stop(hub).await; // router outage
    let mut sent = Vec::new();
    for i in 0..3 {
        sent.push(
            alice
                .send_to(&bob_id, "chat", json!({"n": i}))
                .await
                .unwrap(),
        );
    }
    // Nothing is routed while the router is down.
    assert!(collect(&mut bob_inbox, 1, Duration::from_millis(500))
        .await
        .is_empty());

    let _hub = stack.durable_hub().await;
    let got = collect(&mut bob_inbox, 3, Duration::from_secs(15)).await;
    let ids: Vec<_> = got.iter().map(|e| e.meta.id.clone()).collect();
    assert_eq!(ids, sent, "outage sends routed after restart, in order");
}

// AC2 — DMs to an offline agent arrive, in order, when it reconnects.
#[tokio::test]
async fn offline_dms_are_delivered_in_order_on_reconnect() {
    let _g = STACK_LOCK.lock().await;
    let stack = stack!();
    let _hub = stack.durable_hub().await;
    let bob_id = unique("bob");
    let alice = HubClient::connect(&stack.url, unique("alice").as_str())
        .await
        .unwrap();

    // Bob has never connected: DMs are kept for him anyway.
    let mut sent = Vec::new();
    for i in 0..5 {
        sent.push(
            alice
                .send_to(&bob_id, "chat", json!({"n": i}))
                .await
                .unwrap(),
        );
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    {
        let bob = HubClient::connect(&stack.url, bob_id.as_str())
            .await
            .unwrap();
        let mut inbox = bob.subscribe_inbox().await.unwrap();
        let got = collect(&mut inbox, 5, Duration::from_secs(10)).await;
        let ids: Vec<_> = got.iter().map(|e| e.meta.id.clone()).collect();
        assert_eq!(ids, sent, "first connect: backlog in order");
        tokio::time::sleep(Duration::from_millis(300)).await; // let acks land
        bob.drain().await.unwrap();
    }

    // Offline again; one more DM. On reconnect only the new one arrives
    // (the first five were acked).
    let late = alice
        .send_to(&bob_id, "chat", json!({"n": "late"}))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let bob = HubClient::connect(&stack.url, bob_id.as_str())
        .await
        .unwrap();
    let mut inbox = bob.subscribe_inbox().await.unwrap();
    let got = collect(&mut inbox, 2, Duration::from_secs(3)).await;
    let ids: Vec<_> = got.iter().map(|e| e.meta.id.clone()).collect();
    assert_eq!(ids, vec![late], "reconnect: only the unacked DM");
}

// AC3 — the same meta.id twice is stored once and handled once.
#[tokio::test]
async fn redelivered_envelope_is_handled_once() {
    let _g = STACK_LOCK.lock().await;
    let stack = stack!();
    let _hub = stack.durable_hub().await;
    let bob_id = unique("bob");
    let alice = HubClient::connect(&stack.url, unique("alice").as_str())
        .await
        .unwrap();
    let bob = HubClient::connect(&stack.url, bob_id.as_str())
        .await
        .unwrap();
    let mut inbox = bob.subscribe_inbox().await.unwrap();

    // A publisher retry: the same envelope sent twice through the router.
    let env = Envelope::new(alice.identity(), "chat", MessageKind::Message, json!({})).to(&bob_id);
    alice.send(&env).await.unwrap();
    alice.send(&env).await.unwrap();
    // A consumer-side redelivery: the same envelope stored twice under
    // different stream ids (what a lost ack looks like to the client).
    let twin = Envelope::new(
        alice.identity(),
        "chat",
        MessageKind::Message,
        json!({"t": 1}),
    )
    .to(&bob_id);
    let js = admin_js(&stack.url).await;
    for n in 0..2 {
        let mut h = async_nats::HeaderMap::new();
        h.insert("Nats-Msg-Id", format!("test-{n}").as_str());
        js.publish_with_headers(
            format!("channel.inbox.{bob_id}"),
            h,
            twin.to_json_bytes().unwrap().into(),
        )
        .await
        .unwrap()
        .await
        .unwrap();
    }

    let got = collect(&mut inbox, 4, Duration::from_secs(3)).await;
    let ids: Vec<_> = got.iter().map(|e| e.meta.id.clone()).collect();
    assert_eq!(ids, vec![env.meta.id.clone(), twin.meta.id.clone()]);
    // Router-side: the retried envelope was stored once.
    assert_eq!(
        stored_count(&stack.url, "HUB_INBOX", &format!("channel.inbox.{bob_id}")).await,
        3,
        "1 routed (retry deduped by Nats-Msg-Id) + 2 injected twins"
    );
}

// AC4 — a task result published while the delegator was away is read back
// by `hub-delegate --resume`.
#[tokio::test]
async fn task_result_survives_delegator_disconnect() {
    let _g = STACK_LOCK.lock().await;
    let stack = stack!();
    let _hub = stack.durable_hub().await;
    let worker_id = unique("worker");
    let boss_id = unique("boss");

    // A fake worker: answers the first task it gets.
    let worker = HubClient::connect(&stack.url, worker_id.as_str())
        .await
        .unwrap();
    let mut work = worker.subscribe_inbox().await.unwrap();
    let w = worker.clone();
    let answered = tokio::spawn(async move {
        let task = work.recv().await.expect("task");
        w.send_task_result(&task, TaskOutcome::Done("forty-two".into()))
            .await
            .unwrap();
        task.meta.id
    });

    // Fire and forget: nobody listens on the task channel when the result
    // is published.
    let out = tokio::process::Command::new(support::client_bin("hub-delegate"))
        .args([
            "--nats-url",
            &stack.url,
            "--to",
            &worker_id,
            "--from",
            &boss_id,
        ])
        .args(["--prompt", "what is 6*7", "--no-wait"])
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let task_id = String::from_utf8(out.stdout).unwrap().trim().to_string();
    let answered_id = tokio::time::timeout(Duration::from_secs(10), answered)
        .await
        .expect("worker answered")
        .unwrap();
    assert_eq!(answered_id, task_id);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let out = tokio::process::Command::new(support::client_bin("hub-delegate"))
        .args([
            "--nats-url",
            &stack.url,
            "--to",
            &worker_id,
            "--from",
            &boss_id,
        ])
        .args(["--resume", &task_id, "--timeout", "10"])
        .output()
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "forty-two");
    assert_eq!(
        stored_count(
            &stack.url,
            "HUB_TASKS",
            &format!("channel.{}", task_channel_for(&task_id))
        )
        .await,
        1
    );
}

// AC5 — two hub-servers share the routing consumer: each envelope once.
#[tokio::test]
async fn two_routers_route_each_envelope_once() {
    let _g = STACK_LOCK.lock().await;
    let stack = stack!();
    let _a = stack.durable_hub().await;
    let _b = stack.durable_hub().await;
    let ch = unique("room");
    let bob_id = unique("bob");
    let watcher = HubClient::connect(&stack.url, unique("w").as_str())
        .await
        .unwrap();
    // Raw subscriptions: no client dedupe, so a double route would show.
    let mut room = watcher.subscribe_channel(&ch).await.unwrap();
    let mut dms = watcher
        .subscribe_subject(&format!("channel.inbox.{bob_id}"))
        .await
        .unwrap();
    let alice = HubClient::connect(&stack.url, unique("alice").as_str())
        .await
        .unwrap();
    for i in 0..100 {
        alice.send_message(&ch, json!({"i": i})).await.unwrap();
        alice
            .send_to(&bob_id, "chat", json!({"i": i}))
            .await
            .unwrap();
    }
    let got = collect(&mut room, 200, Duration::from_secs(5)).await;
    assert_eq!(got.len(), 100, "each broadcast routed exactly once");
    let got = collect(&mut dms, 200, Duration::from_secs(5)).await;
    assert_eq!(got.len(), 100, "each DM routed exactly once");
}

// AC6 — without --durable nothing JetStream-related happens.
#[tokio::test]
async fn core_mode_creates_no_streams_and_inbox_stays_live() {
    let _g = STACK_LOCK.lock().await;
    let stack = stack!();
    let _hub = stack.hub(&[]);
    wait_routing(&stack.url).await;
    let js = admin_js(&stack.url).await;
    assert!(
        js.get_stream("HUB_INGRESS").await.is_err(),
        "no ingress stream"
    );
    assert!(js.get_stream("HUB_INBOX").await.is_err(), "no inbox stream");

    let bob_id = unique("bob");
    let bob = HubClient::connect(&stack.url, bob_id.as_str())
        .await
        .unwrap();
    let mut inbox = bob.subscribe_inbox().await.unwrap(); // falls back to core
    let alice = HubClient::connect(&stack.url, unique("alice").as_str())
        .await
        .unwrap();
    let id = alice.send_to(&bob_id, "chat", json!({})).await.unwrap();
    let got = collect(&mut inbox, 1, Duration::from_secs(5)).await;
    assert_eq!(got.first().map(|e| e.meta.id.clone()), Some(id));
}

// Replay guard — a stored envelope bounced into another identity's ingress
// subject (via a JetStream reply subject) is dropped, not routed as them.
#[tokio::test]
async fn replayed_stream_messages_are_not_routed() {
    let _g = STACK_LOCK.lock().await;
    let stack = stack!();
    let _hub = stack.durable_hub().await;
    let ch = unique("t");
    let watcher = HubClient::connect(&stack.url, unique("w").as_str())
        .await
        .unwrap();
    let task_ch = format!("task.{ch}");
    let mut seen = watcher.subscribe_channel(&task_ch).await.unwrap();
    let alice = HubClient::connect(&stack.url, unique("alice").as_str())
        .await
        .unwrap();
    alice
        .send_message(&task_ch, json!({"status": "done"}))
        .await
        .unwrap();
    assert_eq!(collect(&mut seen, 1, Duration::from_secs(5)).await.len(), 1);

    // DIRECT.GET with the reply aimed at bob's ingress subject.
    let raw = async_nats::connect(&stack.url).await.unwrap();
    let req = json!({"seq": 1, "next_by_subj": format!("channel.{task_ch}")});
    raw.publish_with_reply(
        "$JS.API.DIRECT.GET.HUB_TASKS",
        format!("hub.pub.bob.{task_ch}"),
        serde_json::to_vec(&req).unwrap().into(),
    )
    .await
    .unwrap();
    raw.flush().await.unwrap();
    let replays = collect(&mut seen, 1, Duration::from_secs(2)).await;
    assert!(replays.is_empty(), "replay was routed: {replays:?}");
}
