//! Integration test: SurrealDB storage backend.
//!
//! Tests the full lifecycle: migrate → store envelope → query history →
//! store agent → find agent → link reply → get thread.

use chrono::Utc;
use nats_hub::{
    AgentFilter, AgentRecord, Envelope, HistoryQuery, MessageKind, Storage, SurrealStorage,
};
use serde_json::json;

async fn setup() -> SurrealStorage {
    let storage = SurrealStorage::connect_memory().await.unwrap();
    storage.migrate().await.unwrap();
    storage
}

#[tokio::test]
async fn test_store_and_query_envelope() {
    let storage = setup().await;

    let env = Envelope::new(
        "agent-alpha",
        "agents.broadcast",
        MessageKind::Message,
        json!({"text": "hello world"}),
    );

    storage.store_envelope(&env).await.unwrap();

    // Give the async store a moment
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let results = storage
        .query_history(&HistoryQuery::new().channel("agents.broadcast").limit(10))
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].from_identity, "agent-alpha");
    assert_eq!(results[0].channel, "agents.broadcast");
    assert_eq!(results[0].payload["text"], "hello world");
}

#[tokio::test]
async fn test_get_envelope_by_id() {
    let storage = setup().await;

    let env = Envelope::new(
        "agent-beta",
        "agents.tasks",
        MessageKind::Message,
        json!({"task": "compute"}),
    );

    let env_id = env.meta.id.clone();
    storage.store_envelope(&env).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let result = storage.get_envelope(&env_id).await.unwrap();
    assert!(result.is_some());
    let record = result.unwrap();
    assert_eq!(record.from_identity, "agent-beta");
    assert_eq!(record.payload["task"], "compute");
}

#[tokio::test]
async fn test_agent_registry() {
    let storage = setup().await;

    let agent = AgentRecord {
        identity: "agent-gamma".to_string(),
        capabilities: vec!["compute".into(), "observe".into()],
        last_seen: Utc::now(),
        registered_at: Utc::now(),
        metadata: json!({}),
    };

    storage.register_agent(agent).await.unwrap();

    let found = storage.get_agent("agent-gamma").await.unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().capabilities, vec!["compute", "observe"]);

    let agents = storage
        .find_agents(&AgentFilter::new().capabilities(vec!["compute".into()]))
        .await
        .unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].identity, "agent-gamma");
}

#[tokio::test]
async fn test_link_reply_graph_edge() {
    let storage = setup().await;

    let root = Envelope::new(
        "sender",
        "agents.tasks",
        MessageKind::Message,
        json!({"prompt": "compute"}),
    )
    .to("worker-1");
    let root_id = root.meta.id.clone();
    storage.store_envelope(&root).await.unwrap();

    let reply = Envelope::new(
        "worker-1",
        "agents.tasks",
        MessageKind::Message,
        json!({"result": "42"}),
    )
    .to("sender")
    .reply_to(&root_id);
    let reply_id = reply.meta.id.clone();
    storage.store_envelope(&reply).await.unwrap();

    // store_envelope calls link_reply; explicit call must also succeed (idempotent).
    storage
        .link_reply(&reply_id, &root_id)
        .await
        .expect("link_reply should succeed without SurrealQL parse errors");

    // Graph edge direction: reply(in) -> parent(out). Pending clears for recipient.
    let pending = storage.list_pending("worker-1").await.unwrap();
    assert_eq!(pending.len(), 0, "replied message should not be pending");
}

#[tokio::test]
async fn test_conversation_thread() {
    let storage = setup().await;

    // Root message
    let root = Envelope::new(
        "agent-alpha",
        "agents.chat",
        MessageKind::Message,
        json!({"text": "what is 2+2?"}),
    );
    let root_id = root.meta.id.clone();
    storage.store_envelope(&root).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Reply message
    let reply = Envelope::new(
        "agent-beta",
        "agents.chat",
        MessageKind::Message,
        json!({"text": "4"}),
    )
    .reply_to(&root_id);
    storage.store_envelope(&reply).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Get the thread
    let thread = storage.get_thread(&root_id).await.unwrap();
    assert_eq!(thread.len(), 2);
}

#[tokio::test]
async fn test_ping() {
    let storage = setup().await;
    storage.ping().await.unwrap();
}

// ── Schema (v2) ──────────────────────────────────────────────

use chrono::{DateTime, Duration as ChronoDuration};
use nats_hub::storage::surreal::Db;
use nats_hub::{SessionFilter, SessionRecord, WaveRecord, WaveTaskRecord};
use serde::Serialize;
use surrealdb::engine::local::Mem;
use surrealdb::Surreal;

async fn raw_db() -> Db {
    let db: Db = Surreal::new::<Mem>(()).await.unwrap();
    db.use_ns("nats_hub").use_db("messaging").await.unwrap();
    db
}

async fn values(db: &Db, sql: &str) -> Vec<serde_json::Value> {
    db.query(sql)
        .await
        .unwrap()
        .check()
        .unwrap()
        .take(0)
        .unwrap()
}

#[tokio::test]
async fn test_fresh_db_schema_is_applied() {
    let db = raw_db().await;
    let storage = SurrealStorage::from_client(db.clone()).await.unwrap();
    storage.migrate().await.unwrap();
    storage.migrate().await.unwrap(); // idempotent

    // Typed fields are enforced (the old `AT` syntax never applied them).
    let bad = db
        .query(
            "CREATE envelopes:bad SET from_identity = 'a', channel = 'c', kind = 'message', \
             timestamp = 'not-a-date', stored_at = time::now()",
        )
        .await
        .unwrap()
        .check();
    assert!(
        bad.is_err(),
        "string timestamp must be rejected by the schema"
    );

    // Indexes exist.
    let info: surrealdb::Value = db
        .query("INFO FOR TABLE envelopes")
        .await
        .unwrap()
        .take(0)
        .unwrap();
    let info = info.to_string();
    for idx in ["idx_env_to", "idx_env_reply_to", "idx_env_channel_time"] {
        assert!(info.contains(idx), "missing index {idx}: {info}");
    }

    // New writes store native datetimes.
    let env = Envelope::new("a", "c", MessageKind::Message, json!({"k": 1}));
    storage.store_envelope(&env).await.unwrap();
    let is_dt = values(
        &db,
        "SELECT VALUE type::is::datetime(timestamp) FROM envelopes",
    )
    .await;
    assert_eq!(is_dt, vec![json!(true)]);
}

#[tokio::test]
async fn test_session_and_wave_metadata_persisted() {
    let storage = setup().await;
    let now = Utc::now();
    storage
        .create_session(SessionRecord {
            session_id: "s-meta".into(),
            orchestrator: "o".into(),
            worker: "w".into(),
            status: "active".into(),
            cwd: Some("/tmp".into()),
            model: None,
            provider: None,
            created_at: now,
            updated_at: now,
            closed_at: None,
            metadata: json!({"ticket": "L2", "nested": {"n": [1, 2]}}),
        })
        .await
        .unwrap();
    let s = storage.get_session("s-meta").await.unwrap().unwrap();
    assert_eq!(s.metadata["ticket"], "L2");
    assert_eq!(s.metadata["nested"]["n"], json!([1, 2]));
    assert_eq!(s.cwd.as_deref(), Some("/tmp"));
    assert_eq!(s.created_at, now);

    storage
        .create_wave(WaveRecord {
            wave_id: "w-meta".into(),
            goal: "g".into(),
            status: "pending".into(),
            orchestrator: "o".into(),
            created_at: now,
            closed_at: None,
            metadata: json!({"owner": "josh"}),
        })
        .await
        .unwrap();
    let w = storage.get_wave("w-meta").await.unwrap().unwrap();
    assert_eq!(w.metadata["owner"], "josh");
    assert_eq!(w.created_at, now);
}

#[tokio::test]
async fn test_wave_task_optional_fields_round_trip() {
    let storage = setup().await;
    let now = Utc::now();
    storage
        .create_wave_task(WaveTaskRecord {
            wave_id: "w".into(),
            task_id: "t".into(),
            worker: "wk".into(),
            goal: "g".into(),
            status: "pending".into(),
            write_scope: vec!["src/a".into()],
            dependencies: vec![],
            handoff_path: Some("h.md".into()),
            verify_cmd: None,
            created_at: now,
            started_at: None,
            completed_at: None,
            result: None,
        })
        .await
        .unwrap();
    storage
        .update_wave_task_status("w", "t", "done", Some("ok"))
        .await
        .unwrap();
    let t = storage.get_wave_task("w", "t").await.unwrap().unwrap();
    assert_eq!(t.handoff_path.as_deref(), Some("h.md"));
    assert_eq!(t.verify_cmd, None);
    assert_eq!(t.result.as_deref(), Some("ok"));
    assert!(t.completed_at.is_some());
    assert_eq!(t.created_at, now);
}

// ── Legacy (pre-v2) data ─────────────────────────────────────
//
// Row shapes exactly as the pre-change code wrote them: chrono datetimes
// (which the SurrealDB SDK serializes as strings), "" for unset optionals,
// and no session/wave metadata.

#[derive(Serialize)]
struct LegacyAgent {
    identity: String,
    capabilities: Vec<String>,
    last_seen: DateTime<Utc>,
    registered_at: DateTime<Utc>,
    metadata: serde_json::Value,
}

#[derive(Serialize)]
struct LegacyEnvelope {
    from_identity: String,
    channel: String,
    to_identity: Option<String>,
    timestamp: DateTime<Utc>,
    kind: String,
    reply_to: Option<String>,
    payload: serde_json::Value,
    stored_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct LegacySession {
    session_id: String,
    orchestrator: String,
    worker: String,
    status: String,
    cwd: String,
    model: String,
    provider: String,
    created_at: String,
    updated_at: String,
    closed_at: String,
}

#[derive(Serialize)]
struct LegacyWave {
    wave_id: String,
    goal: String,
    status: String,
    orchestrator: String,
    created_at: String,
    closed_at: String,
}

fn legacy_envelope(
    from: &str,
    to: Option<&str>,
    reply_to: Option<&str>,
    ts: DateTime<Utc>,
) -> LegacyEnvelope {
    LegacyEnvelope {
        from_identity: from.into(),
        channel: "tasks".into(),
        to_identity: to.map(String::from),
        timestamp: ts,
        kind: "message".into(),
        reply_to: reply_to.map(String::from),
        payload: json!({"legacy": true}),
        stored_at: ts,
    }
}

/// Populate a DB the way the pre-change code did, including the part of the
/// old migration that did apply (tables + `ON TABLE` indexes).
async fn seed_legacy(db: &Db, created: DateTime<Utc>) {
    db.query(
        "DEFINE TABLE agents SCHEMALESS;
         DEFINE INDEX idx_agents_last_seen ON TABLE agents COLUMNS last_seen;
         DEFINE TABLE envelopes SCHEMALESS;
         DEFINE INDEX idx_env_channel_time ON TABLE envelopes COLUMNS channel, timestamp;
         DEFINE TABLE reply_to SCHEMALESS TYPE RELATION FROM envelopes TO envelopes;
         DEFINE TABLE sessions SCHEMALESS;
         DEFINE TABLE waves SCHEMALESS;
         DEFINE TABLE wave_tasks SCHEMALESS;",
    )
    .await
    .unwrap()
    .check()
    .unwrap();

    // `.content()` return values are ignored: only the stored rows matter.
    let _ = db
        .upsert::<Option<serde::de::IgnoredAny>>(("agents", "old-agent"))
        .content(LegacyAgent {
            identity: "old-agent".into(),
            capabilities: vec!["code".into()],
            last_seen: created,
            registered_at: created,
            metadata: json!({}),
        })
        .await;
    for (id, to, reply_to) in [
        ("old-root", Some("old-agent"), None),
        ("old-open", Some("old-agent"), None),
        ("old-reply", Some("boss"), Some("old-root")),
    ] {
        let from = if reply_to.is_some() {
            "old-agent"
        } else {
            "boss"
        };
        let _ = db
            .create::<Option<serde::de::IgnoredAny>>(("envelopes", id))
            .content(legacy_envelope(from, to, reply_to, created))
            .await;
    }

    let created_s = created.to_rfc3339(); // "+00:00" form, as the old code wrote
    let closed_s = (created + ChronoDuration::minutes(5)).to_rfc3339();
    for (id, c, cl) in [
        ("old-open", created_s.as_str(), ""),
        ("old-closed", created_s.as_str(), closed_s.as_str()),
        ("old-corrupt", "garbage", ""),
    ] {
        let _ = db
            .upsert::<Option<serde::de::IgnoredAny>>(("sessions", id))
            .content(LegacySession {
                session_id: id.into(),
                orchestrator: "josh".into(),
                worker: "w1".into(),
                status: "active".into(),
                cwd: String::new(),
                model: String::new(),
                provider: String::new(),
                created_at: c.into(),
                updated_at: c.into(),
                closed_at: cl.into(),
            })
            .await;
    }
    let _ = db
        .upsert::<Option<serde::de::IgnoredAny>>(("waves", "old-wave"))
        .content(LegacyWave {
            wave_id: "old-wave".into(),
            goal: "g".into(),
            status: "running".into(),
            orchestrator: "josh".into(),
            created_at: created_s.clone(),
            closed_at: String::new(),
        })
        .await;
    db.query(
        "CREATE type::thing('wave_tasks', 'old-wave:t1') SET wave_id = 'old-wave', \
         task_id = 't1', worker = 'w1', goal = 'g', status = 'pending', \
         write_scope = ['src'], dependencies = [], handoff_path = '', verify_cmd = '', \
         created_at = $c, started_at = '', completed_at = '', result = ''",
    )
    .bind(("c", created_s))
    .await
    .unwrap()
    .check()
    .unwrap();
}

#[tokio::test]
async fn test_migrate_converts_legacy_rows() {
    let db = raw_db().await;
    let created: DateTime<Utc> = "2026-08-01T12:00:00.123456789Z".parse().unwrap();
    seed_legacy(&db, created).await;

    // Precondition: the seed really is legacy-shaped.
    let pre = values(
        &db,
        "SELECT VALUE type::is::string(created_at) FROM sessions",
    )
    .await;
    assert_eq!(pre.len(), 3);
    assert!(pre.iter().all(|b| b == &json!(true)));
    let pre = values(
        &db,
        "SELECT VALUE type::is::string(timestamp) FROM envelopes",
    )
    .await;
    assert!(pre.iter().all(|b| b == &json!(true)));

    let storage = SurrealStorage::from_client(db.clone()).await.unwrap();
    storage.migrate().await.unwrap();
    storage.migrate().await.unwrap(); // idempotent on a migrated DB

    // Datetimes converted in place.
    for sql in [
        "SELECT VALUE type::is::datetime(timestamp) FROM envelopes",
        "SELECT VALUE type::is::datetime(stored_at) FROM envelopes",
        "SELECT VALUE type::is::datetime(last_seen) FROM agents",
        "SELECT VALUE type::is::datetime(created_at) FROM waves",
        "SELECT VALUE type::is::datetime(created_at) FROM wave_tasks",
        "SELECT VALUE type::is::datetime(created_at) FROM sessions WHERE session_id != 'old-corrupt'",
    ] {
        let v = values(&db, sql).await;
        assert!(
            !v.is_empty() && v.iter().all(|b| b == &json!(true)),
            "{sql}: {v:?}"
        );
    }

    // Sessions: values preserved, "" → None, metadata defaulted.
    let open = storage.get_session("old-open").await.unwrap().unwrap();
    assert_eq!(open.created_at, created);
    assert_eq!(open.closed_at, None);
    assert_eq!(open.cwd, None);
    assert_eq!(open.metadata, json!({}));
    let closed = storage.get_session("old-closed").await.unwrap().unwrap();
    assert_eq!(closed.closed_at, Some(created + ChronoDuration::minutes(5)));

    // Legacy rows stay updatable under the typed schema.
    storage
        .update_session_status("old-open", "closed")
        .await
        .unwrap();
    let reclosed = storage.get_session("old-open").await.unwrap().unwrap();
    assert!(reclosed.closed_at.is_some());
    storage
        .update_wave_status("old-wave", "completed")
        .await
        .unwrap();
    assert!(storage
        .get_wave("old-wave")
        .await
        .unwrap()
        .unwrap()
        .closed_at
        .is_some());
    storage
        .update_wave_task_status("old-wave", "t1", "running", None)
        .await
        .unwrap();
    let task = storage
        .get_wave_task("old-wave", "t1")
        .await
        .unwrap()
        .unwrap();
    assert!(task.started_at.is_some());
    assert_eq!(task.handoff_path, None);
    assert_eq!(task.created_at, created);
    storage.touch_agent("old-agent").await.unwrap();

    // Agents / envelopes read through the normal paths.
    let alive = storage
        .find_agents(&AgentFilter::new().alive_within(60))
        .await
        .unwrap();
    assert_eq!(alive.len(), 1);
    assert_eq!(alive[0].capabilities, vec!["code"]);
    let hist = storage
        .query_history(&HistoryQuery::new().since(created - ChronoDuration::hours(1)))
        .await
        .unwrap();
    assert_eq!(hist.len(), 3);
    let pending = storage.list_pending("old-agent").await.unwrap();
    assert_eq!(pending.len(), 1, "old-root was answered, old-open was not");
    assert!(pending[0].id.contains("old-open"));

    // A corrupt legacy row is reported, not papered over with "now".
    let listed = storage.list_sessions(&SessionFilter::new()).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|s| s.session_id != "old-corrupt"));
    let err = storage.get_session("old-corrupt").await.unwrap_err();
    assert!(format!("{err:#}").contains("created_at"), "{err:#}");
}
