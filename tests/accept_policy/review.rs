use nats_hub::{ApiClient, Envelope, HubClient, MessageKind};
use serde_json::json;
use std::time::Duration;

#[tokio::test]
async fn foreign_refusal_does_not_end_delegation_and_worker_refusal_exits_five() {
    if std::env::var("NATS_HUB_TEST_MANIFEST").as_deref() != Ok("1") {
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
    for (id, user) in [(&alice, "alice"), (&bob, "bob"), (&worker, "alice")] {
        super::put(
            &writer,
            id,
            user,
            json!({"from":"owner"}),
            9_000_000_000_000,
        )
        .await;
    }
    let w = HubClient::connect(&url, &worker).await.unwrap();
    let b = HubClient::connect(&url, &bob).await.unwrap();
    let mut inbox = w.subscribe_inbox().await.unwrap();
    for worker_refusal in [false, true] {
        let mut child = tokio::process::Command::new("target/debug/hub-delegate")
            .args([
                "--nats-url",
                &url,
                "--from",
                &alice,
                "--to",
                &worker,
                "--prompt",
                "hello",
                "--timeout",
                "5",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let dm = tokio::time::timeout(Duration::from_secs(3), inbox.recv())
            .await
            .unwrap()
            .unwrap();
        // Reuse the victim's dispatch ID and channel in another sender's refused DM.
        let mut attack = Envelope::new(
            &bob,
            &dm.meta.channel,
            MessageKind::Message,
            dm.payload.clone(),
        )
        .to(&worker);
        attack.meta.id = dm.meta.id.clone();
        b.send(&attack).await.unwrap();
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(child.try_wait().unwrap().is_none());
        let mut result = Envelope::new(
            &worker,
            &dm.meta.channel,
            MessageKind::Message,
            json!({"task_id":dm.meta.id,"status":if worker_refusal {"error"} else {"done"},
                "result":"ok","error":"refused by worker accept policy","rejected":worker_refusal,
                "refused_sender":alice,"message_id":dm.meta.id}),
        );
        let mut foreign_worker_refusal = result.clone();
        foreign_worker_refusal.meta.id = uuid::Uuid::new_v4().to_string();
        foreign_worker_refusal.payload["rejected"] = true.into();
        foreign_worker_refusal.payload["refused_sender"] = bob.clone().into();
        foreign_worker_refusal.payload["message_id"] = "foreign-message".into();
        w.send(&foreign_worker_refusal).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(child.try_wait().unwrap().is_none());
        result.meta.reply_to = Some(dm.meta.id);
        w.send(&result).await.unwrap();
        let output = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if worker_refusal { 5 } else { 0 }),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = tokio::process::Command::new("target/debug/hub-session")
        .env("NATS_HUB_IDENTITY", &bob)
        .args([
            "create",
            "--nats-url",
            &url,
            "--from",
            &bob,
            "--worker",
            &worker,
            "--prompt",
            "hello",
            "--timeout",
            "3",
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
    let api = ApiClient::connect_with_identity(&url, &bob).await.unwrap();
    let sessions = api
        .request("session.list", json!({"worker":worker}))
        .await
        .unwrap();
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn invalid_ids_do_not_crash_or_stall_routing() {
    if std::env::var("NATS_HUB_TEST_MANIFEST").as_deref() != Ok("1") {
        return;
    }
    let url = std::env::var("NATS_URL").unwrap();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let sender = format!("sender-{suffix}");
    let worker = format!("worker-{suffix}");
    let writer = ApiClient::connect_with_identity(&url, "test-manifest")
        .await
        .unwrap();
    for id in [&sender, &worker] {
        super::put(
            &writer,
            id,
            "alice",
            json!({"from":"owner"}),
            9_000_000_000_000,
        )
        .await;
    }
    let w = HubClient::connect(&url, &worker).await.unwrap();
    let mut inbox = w.subscribe_inbox().await.unwrap();
    let raw = async_nats::connect(&url).await.unwrap();
    let channel = format!("task.{suffix}");
    for bad in ["\r\n", "", &"x".repeat(129)] {
        for reply in [false, true] {
            let mut env = Envelope::new(
                &sender,
                &channel,
                MessageKind::Message,
                json!({"prompt":"bad"}),
            )
            .to(&worker);
            if reply {
                env.meta.reply_to = Some(bad.into());
            } else {
                env.meta.id = bad.into();
            }
            raw.publish(
                format!("hub.pub.{sender}.{channel}"),
                env.to_json_bytes().unwrap().into(),
            )
            .await
            .unwrap();
        }
    }
    let good = Envelope::new(
        &sender,
        &channel,
        MessageKind::Message,
        json!({"prompt":"good"}),
    )
    .to(&worker);
    raw.publish(
        format!("hub.pub.{sender}.{channel}"),
        good.to_json_bytes().unwrap().into(),
    )
    .await
    .unwrap();
    raw.flush().await.unwrap();
    let delivered = tokio::time::timeout(Duration::from_secs(3), inbox.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.meta.id, good.meta.id);
    assert!(
        tokio::time::timeout(Duration::from_millis(150), inbox.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn writer_requires_bound_identity_at_startup() {
    let output = tokio::process::Command::new("target/debug/hub-server")
        .args(["--manifest-writer", "writer"])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires --require-bound-identity"));
}
