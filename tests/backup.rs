//! Backup export → restore import round-trip, and `backup.export`
//! authorization.
//!
//! The round-trip covers envelopes (plus a reply thread), agents,
//! sessions, waves and wave tasks: export a live `SurrealStorage` (the
//! same engine hub-server holds open while running), then import into a
//! fresh SurrealDB and verify every record type came back.

use std::sync::Arc;

use chrono::Utc;
use nats_hub::query_api::{handle_request_authorized, ApiAuthz};
use nats_hub::storage::{backup, SurrealStorage};
use nats_hub::{
    AgentRecord, Envelope, MessageKind, SessionRecord, Storage, WaveRecord, WaveTaskRecord,
};
use serde_json::json;
use tempfile::TempDir;

async fn seed(storage: &SurrealStorage) -> (String, String) {
    storage.migrate().await.unwrap();
    storage
        .register_agent(AgentRecord {
            identity: "w1".into(),
            capabilities: vec!["compute".into()],
            last_seen: Utc::now(),
            registered_at: Utc::now(),
            metadata: json!({}),
        })
        .await
        .unwrap();

    let parent = Envelope::new("a", "chan.a", MessageKind::Message, json!({"q": 1}));
    let parent_id = parent.meta.id.clone();
    let reply =
        Envelope::new("b", "chan.a", MessageKind::Message, json!({"a": 2})).reply_to(&parent_id);
    let reply_id = reply.meta.id.clone();
    storage.store_envelope(&parent).await.unwrap();
    storage.store_envelope(&reply).await.unwrap();

    let now = Utc::now();
    storage
        .create_session(SessionRecord {
            session_id: "s1".into(),
            orchestrator: "orch".into(),
            worker: "w1".into(),
            status: "active".into(),
            cwd: None,
            model: None,
            provider: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
            metadata: json!({}),
            backend_ctx: None,
        })
        .await
        .unwrap();
    storage
        .create_wave(WaveRecord {
            wave_id: "w1".into(),
            goal: "g".into(),
            status: "running".into(),
            orchestrator: "orch".into(),
            created_at: now,
            closed_at: None,
            metadata: json!({}),
        })
        .await
        .unwrap();
    storage
        .create_wave_task(WaveTaskRecord {
            wave_id: "w1".into(),
            task_id: "t1".into(),
            worker: "w1".into(),
            goal: "g".into(),
            status: "pending".into(),
            write_scope: vec!["src/".into()],
            dependencies: vec![],
            handoff_path: None,
            verify_cmd: None,
            created_at: now,
            started_at: None,
            completed_at: None,
            result: None,
            verify_result: None,
        })
        .await
        .unwrap();
    (parent_id, reply_id)
}

#[tokio::test]
async fn backup_restore_roundtrip() {
    let dir_src = TempDir::new().unwrap();
    let dir_dst = TempDir::new().unwrap();
    let export = dir_src.path().join("hub.surql");

    let src = SurrealStorage::connect(dir_src.path().join("db").to_str().unwrap())
        .await
        .unwrap();
    let (parent_id, reply_id) = seed(&src).await;

    // Consistent export while the source storage is live (mirrors the
    // running hub-server holding its DB open).
    let bytes = backup::export_to(&src.db(), &export).await.unwrap();
    assert!(bytes > 0);
    let second = backup::export_to(&src.db(), &export).await;
    assert!(second.is_err(), "must refuse to overwrite: {second:?}");

    // Restore into a FRESH path: no migrate first — the file brings its
    // own DEFINEs.
    let dst = SurrealStorage::connect(dir_dst.path().join("db").to_str().unwrap())
        .await
        .unwrap();
    backup::import_from(&dst.db(), &export).await.unwrap();
    dst.migrate().await.unwrap();

    let counts = backup::table_counts(&dst.db()).await.unwrap();
    let get = |t: &str| counts.iter().find(|(n, _)| n == t).map(|(_, n)| *n);
    assert_eq!(get("envelopes"), Some(2));
    assert_eq!(get("agents"), Some(1));
    assert_eq!(get("sessions"), Some(1));
    assert_eq!(get("waves"), Some(1));
    assert_eq!(get("wave_tasks"), Some(1));

    // Record-level checks: every type survived intact.
    let agent = dst.get_agent("w1").await.unwrap().unwrap();
    assert_eq!(agent.capabilities, vec!["compute"]);
    let env = dst.get_envelope(&parent_id).await.unwrap().unwrap();
    assert_eq!(env.payload["q"], 1);
    let thread = dst.get_thread(&parent_id).await.unwrap();
    assert_eq!(
        thread.len(),
        2,
        "the reply_to edge survived export → import"
    );
    assert!(thread
        .iter()
        .any(|e| e.id.contains(&reply_id) || e.id == reply_id || e.id.ends_with(&reply_id)));
    let sess = dst.get_session("s1").await.unwrap().unwrap();
    assert_eq!(sess.status, "active");
    let wave = dst.get_wave("w1").await.unwrap().unwrap();
    assert_eq!(wave.status, "running");
    let task = dst.get_wave_task("w1", "t1").await.unwrap().unwrap();
    assert_eq!(task.write_scope, vec!["src/"]);
}

#[tokio::test]
async fn backup_export_is_admin_only_and_confined_to_backup_dir() {
    let dir = TempDir::new().unwrap();
    let storage: Arc<dyn Storage> = Arc::new(SurrealStorage::connect_memory().await.unwrap());
    storage.migrate().await.unwrap();
    let req = |name: &str| {
        serde_json::to_vec(&json!({"op": "backup.export", "params": {"name": name}})).unwrap()
    };

    let authz = ApiAuthz {
        require_bound: false,
        admins: ["ops".to_string()].into_iter().collect(),
        backup_dir: Some(dir.path().to_path_buf()),
    };

    // Bound non-admin caller → denied (write op, no ownership rule).
    let resp = handle_request_authorized(
        &storage,
        "hub.api.mallory.backup.export",
        &req("b.surql"),
        &authz,
    )
    .await;
    assert!(!resp.ok, "non-admin must be denied: {resp:?}");

    // Anonymous legacy caller on an enforcing hub → denied.
    let resp =
        handle_request_authorized(&storage, "hub.api.backup.export", &req("b.surql"), &authz).await;
    assert!(!resp.ok, "legacy caller must be denied: {resp:?}");

    // Admin → allowed; the export lands inside --backup-dir.
    let resp = handle_request_authorized(
        &storage,
        "hub.api.ops.backup.export",
        &req("b.surql"),
        &authz,
    )
    .await;
    assert!(resp.ok, "admin export failed: {resp:?}");
    assert!(dir.path().join("b.surql").is_file());

    // Existing file → refused (never overwrites).
    let resp = handle_request_authorized(
        &storage,
        "hub.api.ops.backup.export",
        &req("b.surql"),
        &authz,
    )
    .await;
    assert!(!resp.ok, "overwrite must be refused: {resp:?}");

    // Paths, traversal and hidden names → refused, even for an admin.
    let outside = dir.path().parent().unwrap().join("escaped.surql");
    for bad in [
        "../escaped.surql",
        "/tmp/x.surql",
        "a/b.surql",
        ".hidden",
        "",
    ] {
        let resp =
            handle_request_authorized(&storage, "hub.api.ops.backup.export", &req(bad), &authz)
                .await;
        assert!(!resp.ok, "{bad:?} must be refused: {resp:?}");
    }
    assert!(!outside.exists());

    // Missing name → handler-level error (still admin-gated first).
    let resp = handle_request_authorized(
        &storage,
        "hub.api.ops.backup.export",
        &serde_json::to_vec(&json!({"op": "backup.export", "params": {}})).unwrap(),
        &authz,
    )
    .await;
    assert!(!resp.ok && resp.error.unwrap().contains("name"));

    // A permissive hub allows the op (as every op), but still only inside
    // --backup-dir…
    let permissive = ApiAuthz {
        backup_dir: Some(dir.path().to_path_buf()),
        ..ApiAuthz::permissive()
    };
    let resp = handle_request_authorized(
        &storage,
        "hub.api.backup.export",
        &req("b2.surql"),
        &permissive,
    )
    .await;
    assert!(resp.ok, "permissive hub export failed: {resp:?}");

    // …and without --backup-dir the op is disabled for everyone.
    let resp = handle_request_authorized(
        &storage,
        "hub.api.backup.export",
        &req("b3.surql"),
        &ApiAuthz::permissive(),
    )
    .await;
    assert!(!resp.ok && resp.error.unwrap().contains("--backup-dir"));
}
