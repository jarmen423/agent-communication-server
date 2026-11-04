//! Live refusal contract on a manifest-writer stack.
use nats_hub_client::{ApiClient, Envelope, HubClient, MessageKind};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn gate_delivery_notice_terminal_and_history() {
    if std::env::var("NATS_HUB_TEST_MANIFEST").as_deref() != Ok("1") {
        eprintln!("Skipping: requires NATS_HUB_TEST_MANIFEST=1");
        return;
    }
    let url = std::env::var("NATS_URL").unwrap();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let alice = format!("alice-{suffix}");
    let bob = format!("bob-{suffix}");
    let worker = format!("worker-{suffix}");
    let writer = ApiClient::connect_with_identity(&url, "test-manifest")
        .await
        .unwrap();
    let rev = 9_000_000_000_000i64;
    for (id, user) in [(&alice, "alice"), (&bob, "bob"), (&worker, "alice")] {
        let _r = writer.request("manifest.put",json!({"record":{"identity":id,"owner_kind":"user","user":user,"agent":"echo","accept":{"from":"owner"},"rev":rev}})).await.unwrap();
    }
    let a = HubClient::connect(&url, &alice).await.unwrap();
    let b = HubClient::connect(&url, &bob).await.unwrap();
    let w = HubClient::connect(&url, &worker).await.unwrap();
    let mut inbox = w.subscribe_inbox().await.unwrap();
    let mut notices = b.subscribe_inbox().await.unwrap();
    let channel = format!("task.{suffix}");
    let mut terminals = b.subscribe_channel(&channel).await.unwrap();
    let refused = Envelope::new(
        &bob,
        &channel,
        MessageKind::Message,
        json!({"prompt":"no","task_channel":channel}),
    )
    .to(&worker);
    b.send(&refused).await.unwrap();
    let notice = tokio::time::timeout(Duration::from_secs(2), notices.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(nats_hub_client::accept::is_rejection(&notice, &refused.meta.id, &bob).is_some());
    let terminal = tokio::time::timeout(Duration::from_secs(2), terminals.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        nats_hub_client::accept::is_rejected_terminal(&terminal, &refused.meta.id, &bob).is_some()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(150), inbox.recv())
            .await
            .is_err()
    );
    if std::env::var("NATS_HUB_DURABLE").is_ok() {
        let raw = async_nats::connect(&url).await.unwrap();
        raw.publish(
            format!("hub.pub.{bob}.{channel}"),
            refused.to_json_bytes().unwrap().into(),
        )
        .await
        .unwrap();
        raw.flush().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(300), notices.recv())
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(300), terminals.recv())
                .await
                .is_err()
        );
        use futures_util::StreamExt;
        let stream = async_nats::jetstream::new(raw)
            .get_stream("HUB_TASKS")
            .await
            .unwrap();
        let consumer = stream
            .create_consumer(async_nats::jetstream::consumer::pull::Config {
                filter_subject: format!("channel.{channel}"),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut messages = consumer
            .fetch()
            .max_messages(10)
            .expires(Duration::from_millis(300))
            .messages()
            .await
            .unwrap();
        let mut stored = 0;
        while let Some(message) = messages.next().await {
            message.unwrap();
            stored += 1;
        }
        assert_eq!(stored, 1);
    }
    let allowed = Envelope::new(
        &alice,
        &channel,
        MessageKind::Message,
        json!({"prompt":"yes"}),
    )
    .to(&worker);
    a.send(&allowed).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), inbox.recv())
            .await
            .unwrap()
            .unwrap()
            .meta
            .id,
        allowed.meta.id
    );
    let api = ApiClient::connect_with_identity(&url, &bob).await.unwrap();
    let history = api
        .request("envelope.get", json!({"id":refused.meta.id}))
        .await;
    assert!(history.is_err() || history.unwrap()["envelope"].is_null());
}

#[test]
fn terminal_channels_cannot_address_inboxes() {
    for c in [
        "inbox.victim",
        "task.a.more",
        "wave.w.task.t.more",
        "task.*",
        "task.",
    ] {
        assert!(!nats_hub_client::accept::task_channel(c));
    }
    for c in ["task.a", "wave.w.task.t"] {
        assert!(nats_hub_client::accept::task_channel(c));
    }
}

#[tokio::test]
async fn policies_replies_cli_and_waves() {
    if std::env::var("NATS_HUB_TEST_MANIFEST").as_deref() != Ok("1") {
        return;
    }
    let url = std::env::var("NATS_URL").unwrap();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let worker = format!("wp-{suffix}");
    let alice = format!("ap-{suffix}");
    let bob = format!("bp-{suffix}");
    let unknown = format!("up-{suffix}");
    let writer = ApiClient::connect_with_identity(&url, "test-manifest")
        .await
        .unwrap();
    for (id, user) in [(&worker, "alice"), (&alice, "alice"), (&bob, "bob")] {
        put(
            &writer,
            id,
            user,
            json!({"from":"owner"}),
            9_000_000_000_000,
        )
        .await;
    }
    let a = HubClient::connect(&url, &alice).await.unwrap();
    let b = HubClient::connect(&url, &bob).await.unwrap();
    let w = HubClient::connect(&url, &worker).await.unwrap();
    let u = HubClient::connect(&url, &unknown).await.unwrap();
    let mut inbox = w.subscribe_inbox().await.unwrap();
    // A received DM is the only authority for the reverse reply exemption.
    let mut binbox = b.subscribe_inbox().await.unwrap();
    put(
        &writer,
        &bob,
        "bob",
        json!({"from":"tenant"}),
        9_000_000_000_001,
    )
    .await;
    let original = Envelope::new(&worker, "chat", MessageKind::Message, json!({})).to(&bob);
    w.send(&original).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), binbox.recv())
        .await
        .unwrap()
        .unwrap();
    let mut reply = Envelope::new(&bob, "chat", MessageKind::Message, json!({})).to(&worker);
    reply.meta.reply_to = Some(original.meta.id.clone());
    b.send(&reply).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), inbox.recv())
            .await
            .unwrap()
            .unwrap()
            .meta
            .id,
        reply.meta.id
    );

    put(
        &writer,
        &worker,
        "alice",
        json!({"from":"users","users":["bob"]}),
        9_000_000_000_001,
    )
    .await;
    delivered(&b, &worker, &mut inbox).await;
    put(
        &writer,
        &worker,
        "alice",
        json!({"from":"tenant"}),
        9_000_000_000_002,
    )
    .await;
    delivered(&a, &worker, &mut inbox).await;
    let mut unotice = u.subscribe_inbox().await.unwrap();
    let env = Envelope::new(&unknown, "chat", MessageKind::Message, json!({})).to(&worker);
    u.send(&env).await.unwrap();
    assert!(nats_hub_client::accept::is_rejection(
        &tokio::time::timeout(Duration::from_secs(2), unotice.recv())
            .await
            .unwrap()
            .unwrap(),
        &env.meta.id,
        &unknown
    )
    .is_some());

    put(
        &writer,
        &worker,
        "alice",
        json!({"from":"owner"}),
        9_000_000_000_003,
    )
    .await;
    let start = std::time::Instant::now();
    let output = tokio::process::Command::new(
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("hub-delegate"),
    )
    .args([
        "--nats-url",
        &url,
        "--from",
        &bob,
        "--to",
        &worker,
        "--prompt",
        "refuse",
        "--timeout",
        "10",
    ])
    .output()
    .await
    .unwrap();
    assert_eq!(
        output.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(start.elapsed() < Duration::from_secs(2));

    for (owner, allowed) in [(&bob, false), (&alice, true)] {
        let api = ApiClient::connect_with_identity(&url, owner).await.unwrap();
        let wave = format!("wave-{}", uuid::Uuid::new_v4().simple());
        api.request("wave.create",json!({"wave":{"wave_id":wave,"goal":"check","status":"pending","orchestrator":owner,"created_at":chrono::Utc::now().to_rfc3339()},"tasks":[{"task_id":"t","worker":worker,"goal":"check"}]})).await.unwrap();
        api.request("wave.spawn", json!({"wave_id":wave,"timeout_secs":30}))
            .await
            .unwrap();
        if allowed {
            // Ignore teardown traffic from the failed wave.
            loop {
                let dm = tokio::time::timeout(Duration::from_secs(5), inbox.recv())
                    .await
                    .unwrap()
                    .unwrap();
                if dm.payload["action"] == "session_start" && dm.payload["wave_id"] == wave {
                    assert_eq!(
                        dm.meta.origin.unwrap().on_behalf_of.unwrap().identity,
                        *owner
                    );
                    let channel = format!("wave.{wave}.task.t");
                    let attack = Envelope::new(
                        &bob,
                        &channel,
                        MessageKind::Message,
                        json!({"prompt":"attack", "task_channel":channel, "task_id":"t"}),
                    )
                    .to(&worker);
                    let mut attack = attack;
                    attack.meta.id = dm.meta.id.clone();
                    b.send(&attack).await.unwrap();
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let state = api
                        .request("wave.status", json!({"wave_id":wave}))
                        .await
                        .unwrap();
                    assert_eq!(state["wave"]["status"], "running");
                    w.send(&Envelope::new(
                        &worker,
                        channel,
                        MessageKind::Message,
                        json!({"status":"done","result":"ok"}),
                    ))
                    .await
                    .unwrap();
                    break;
                }
            }
        }
        let mut final_status = String::new();
        for _ in 0..50 {
            let status = api
                .request("wave.status", json!({"wave_id":wave}))
                .await
                .unwrap();
            final_status = status["wave"]["status"].as_str().unwrap_or_default().into();
            if ["failed", "completed"].contains(&final_status.as_str()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(final_status, if allowed { "completed" } else { "failed" });
    }
    writer
        .request(
            "manifest.remove",
            json!({"identity":worker,"rev":9_000_000_000_004i64}),
        )
        .await
        .unwrap();
    let mut anotice = a.subscribe_inbox().await.unwrap();
    let env = Envelope::new(&alice, "chat", MessageKind::Message, json!({})).to(&worker);
    a.send(&env).await.unwrap();
    assert!(nats_hub_client::accept::is_rejection(
        &tokio::time::timeout(Duration::from_secs(2), anotice.recv())
            .await
            .unwrap()
            .unwrap(),
        &env.meta.id,
        &alice
    )
    .is_some());
}
async fn put(api: &ApiClient, id: &str, user: &str, accept: serde_json::Value, rev: i64) {
    api.request("manifest.put",json!({"record":{"identity":id,"owner_kind":"user","user":user,"agent":"echo","accept":accept,"rev":rev}})).await.unwrap();
}
async fn delivered(
    client: &HubClient,
    target: &str,
    inbox: &mut tokio::sync::mpsc::UnboundedReceiver<Envelope>,
) {
    let env = Envelope::new(client.identity(), "chat", MessageKind::Message, json!({})).to(target);
    client.send(&env).await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), inbox.recv())
            .await
            .unwrap()
            .unwrap()
            .meta
            .id,
        env.meta.id
    );
}

#[path = "accept_policy/review.rs"]
mod review;
