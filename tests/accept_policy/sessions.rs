//! Session refusals and live inbox isolation on a writer-enabled stack.
use super::*;

async fn identities() -> Option<(String, String, String, HubClient, ApiClient)> {
    if std::env::var("NATS_HUB_TEST_MANIFEST").as_deref() != Ok("1") {
        return None;
    }
    let url = std::env::var("NATS_URL").unwrap();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let owner = format!("session-owner-{suffix}");
    let worker = format!("session-worker-{suffix}");
    let writer = ApiClient::connect_with_identity(&url, "test-manifest")
        .await
        .unwrap();
    for id in [&owner, &worker] {
        writer
            .request(
                "manifest.put",
                json!({"record": {"identity":id,
            "owner_kind":"user", "user":"alice", "agent":"echo", "rev":9_000_000_000_000i64}}),
            )
            .await
            .unwrap();
    }
    let client = HubClient::connect(&url, &owner).await.unwrap();
    let api = ApiClient::connect_with_identity(&url, &owner)
        .await
        .unwrap();
    Some((url, owner, worker, client, api))
}

#[tokio::test]
async fn session_timeout_persists_record_without_consuming_pending_inbox() {
    let Some((url, owner, worker, client, api)) = identities().await else {
        return;
    };
    let pending = Envelope::new(
        &owner,
        "chat",
        MessageKind::Message,
        json!({"pending":true}),
    )
    .to(&owner);
    client.send(&pending).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let output = tokio::process::Command::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/hub-session"),
    )
    .args([
        "--nats-url",
        &url,
        "create",
        "--worker",
        &worker,
        "--from",
        &owner,
        "--prompt",
        "hello",
        "--timeout",
        "1",
    ])
    .output()
    .await
    .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sid = String::from_utf8(output.stdout).unwrap();
    let record = api
        .request("session.get", json!({"session_id":sid.trim()}))
        .await
        .unwrap();
    assert_eq!(record["session"]["status"], "active");
    let mut inbox = client.subscribe_inbox().await.unwrap();
    if std::env::var("NATS_HUB_DURABLE").as_deref() == Ok("1") {
        let message = tokio::time::timeout(Duration::from_secs(3), inbox.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.meta.id, pending.meta.id);
    }
}

#[tokio::test]
async fn worker_session_refusal_exits_five_without_record() {
    let Some((url, owner, worker, _, api)) = identities().await else {
        return;
    };
    let worker_client = HubClient::connect(&url, &worker).await.unwrap();
    let mut inbox = worker_client.subscribe_inbox_live().await.unwrap();
    let process = tokio::process::Command::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/hub-session"),
    )
    .args([
        "--nats-url",
        &url,
        "create",
        "--worker",
        &worker,
        "--from",
        &owner,
        "--prompt",
        "hello",
        "--timeout",
        "5",
    ])
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
    let start = tokio::time::timeout(Duration::from_secs(3), inbox.recv())
        .await
        .unwrap()
        .unwrap();
    let mut refusal = Envelope::new(
        &worker,
        &start.meta.channel,
        MessageKind::Status,
        json!({"status":"error", "rejected":true, "refused_sender":owner,
               "message_id":start.meta.id, "task_id":start.payload["session_id"], "error":"refused"}),
    );
    refusal.meta.reply_to = Some(start.meta.id.clone());
    worker_client.send(&refusal).await.unwrap();
    let output = process.wait_with_output().await.unwrap();
    assert_eq!(
        output.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let error = api
        .request(
            "session.get",
            json!({"session_id":start.payload["session_id"]}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("session not found"));
}
