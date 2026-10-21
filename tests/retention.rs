//! Integration tests for the retention pruner (`storage::retention`).
//!
//! Covers the acceptance criteria: age rule, size cap, the "keep live
//! state" rule (live sessions/waves and their channels are never pruned),
//! terminal (longer) retention for closed sessions/waves, and draining in
//! bounded batches. The per-statement batch bound itself is a unit test
//! in `src/storage/retention.rs`.

use std::time::Duration;

use chrono::Utc;
use nats_hub::protocol::{Envelope, MessageKind, Meta};
use nats_hub::storage::retention::{run_pass, RetentionPolicy};
use nats_hub::{
    HistoryQuery, SessionFilter, SessionRecord, Storage, SurrealStorage, WaveRecord, WaveTaskRecord,
};
use serde_json::json;

async fn setup() -> SurrealStorage {
    let storage = SurrealStorage::connect_memory().await.unwrap();
    storage.migrate().await.unwrap();
    storage
}

/// Envelope with a controlled timestamp.
fn env_at(from: &str, channel: &str, ts: chrono::DateTime<Utc>) -> Envelope {
    Envelope {
        meta: Meta {
            id: uuid::Uuid::new_v4().to_string(),
            from: from.to_string(),
            channel: channel.to_string(),
            to: None,
            timestamp: ts,
            kind: MessageKind::Message,
            reply_to: None,
        },
        payload: json!({}),
    }
}

fn session(id: &str, status: &str, closed_at: Option<chrono::DateTime<Utc>>) -> SessionRecord {
    let now = Utc::now();
    SessionRecord {
        session_id: id.to_string(),
        orchestrator: "orch".into(),
        worker: "w".into(),
        status: status.to_string(),
        cwd: None,
        model: None,
        provider: None,
        created_at: now - chrono::Duration::days(60),
        updated_at: closed_at.unwrap_or(now),
        closed_at,
        metadata: json!({}),
        backend_ctx: None,
    }
}

fn wave(id: &str, status: &str, closed_at: Option<chrono::DateTime<Utc>>) -> WaveRecord {
    WaveRecord {
        wave_id: id.to_string(),
        goal: "g".into(),
        status: status.to_string(),
        orchestrator: "orch".into(),
        created_at: Utc::now() - chrono::Duration::days(60),
        closed_at,
        metadata: json!({}),
    }
}

async fn envelope_count(storage: &SurrealStorage) -> usize {
    storage
        .query_history(&HistoryQuery::new().limit(100_000))
        .await
        .unwrap()
        .len()
}

fn policy() -> RetentionPolicy {
    RetentionPolicy {
        batch_size: 10,
        ..RetentionPolicy::disabled()
    }
}

#[tokio::test]
async fn age_rule_deletes_old_envelopes() {
    let s = setup().await;
    let now = Utc::now();
    s.store_envelope(&env_at("a", "chan.a", now - chrono::Duration::days(10)))
        .await
        .unwrap();
    s.store_envelope(&env_at("a", "chan.a", now - chrono::Duration::days(2)))
        .await
        .unwrap();
    s.store_envelope(&env_at("a", "chan.a", now)).await.unwrap();

    let mut p = policy();
    p.max_envelope_age = Some(Duration::from_secs(5 * 86_400));
    let r = run_pass(&s, &p).await.unwrap();

    assert_eq!(r.envelopes_age, 1);
    assert_eq!(envelope_count(&s).await, 2);
}

#[tokio::test]
async fn size_cap_keeps_newest() {
    let s = setup().await;
    let base = Utc::now() - chrono::Duration::hours(1);
    for i in 0..20 {
        s.store_envelope(&env_at("a", "chan.a", base + chrono::Duration::seconds(i)))
            .await
            .unwrap();
    }

    let mut p = policy();
    p.max_envelopes = Some(5);
    let r = run_pass(&s, &p).await.unwrap();

    assert_eq!(r.envelopes_cap, 15);
    let left = s
        .query_history(&HistoryQuery::new().limit(100))
        .await
        .unwrap();
    assert_eq!(left.len(), 5);
    // The survivors are the newest (query_history is newest-first).
    let oldest_kept = left.last().unwrap().timestamp;
    assert!(oldest_kept > base + chrono::Duration::seconds(14));
}

#[tokio::test]
async fn live_state_is_never_pruned() {
    let s = setup().await;
    let old = Utc::now() - chrono::Duration::days(60);

    // An open session and a running wave, both with very old envelopes.
    s.create_session(session("s1", "active", None))
        .await
        .unwrap();
    s.create_wave(wave("w1", "running", None)).await.unwrap();
    s.create_wave_task(WaveTaskRecord {
        wave_id: "w1".into(),
        task_id: "t1".into(),
        worker: "w".into(),
        goal: "g".into(),
        status: "running".into(),
        write_scope: vec![],
        dependencies: vec![],
        handoff_path: None,
        verify_cmd: None,
        created_at: old,
        started_at: None,
        completed_at: None,
        result: None,
        verify_result: None,
    })
    .await
    .unwrap();

    for ch in [
        "session.s1",      // live session channel
        "wave.w1",         // live wave channel
        "wave.w1.task.t1", // wave task channel
        "session.w1--t1",  // wave-task session channel
        "other.chan",      // unprotected
        "inbox.w1",        // unprotected
    ] {
        s.store_envelope(&env_at("a", ch, old)).await.unwrap();
    }

    let mut p = policy();
    p.max_envelope_age = Some(Duration::from_secs(86_400));
    p.terminal_retention = Some(Duration::from_secs(30 * 86_400));
    p.max_envelopes = Some(2); // cap can't reach protected rows either
    let r = run_pass(&s, &p).await.unwrap();

    assert_eq!(r.envelopes_age, 2, "only other.chan + inbox.w1 go");
    assert_eq!(envelope_count(&s).await, 4, "live channels all survive");
    assert!(s.get_session("s1").await.unwrap().is_some());
    assert!(s.get_wave("w1").await.unwrap().is_some());
    assert!(s.get_wave_task("w1", "t1").await.unwrap().is_some());
}

#[tokio::test]
async fn terminal_state_gets_longer_retention() {
    let s = setup().await;
    let now = Utc::now();
    let old_env = now - chrono::Duration::days(40);

    // Session closed 10d ago: inside the 30d terminal window → kept, and
    // its 40d-old channel envelopes survive the 5d age rule.
    s.create_session(session(
        "s-recent",
        "closed",
        Some(now - chrono::Duration::days(10)),
    ))
    .await
    .unwrap();
    s.store_envelope(&env_at("a", "session.s-recent", old_env))
        .await
        .unwrap();

    // Session closed 40d ago: past the window → record + channel go.
    s.create_session(session(
        "s-old",
        "closed",
        Some(now - chrono::Duration::days(40)),
    ))
    .await
    .unwrap();
    s.store_envelope(&env_at("a", "session.s-old", old_env))
        .await
        .unwrap();

    // Wave completed 40d ago + task → wave, task and wave channels go.
    s.create_wave(wave(
        "w-old",
        "completed",
        Some(now - chrono::Duration::days(40)),
    ))
    .await
    .unwrap();
    s.create_wave_task(WaveTaskRecord {
        wave_id: "w-old".into(),
        task_id: "t1".into(),
        worker: "w".into(),
        goal: "g".into(),
        status: "done".into(),
        write_scope: vec![],
        dependencies: vec![],
        handoff_path: None,
        verify_cmd: None,
        created_at: old_env,
        started_at: None,
        completed_at: Some(now - chrono::Duration::days(40)),
        result: None,
        verify_result: None,
    })
    .await
    .unwrap();
    s.store_envelope(&env_at("a", "wave.w-old", old_env))
        .await
        .unwrap();
    s.store_envelope(&env_at("a", "session.w-old--t1", old_env))
        .await
        .unwrap();

    let mut p = policy();
    p.max_envelope_age = Some(Duration::from_secs(5 * 86_400));
    p.terminal_retention = Some(Duration::from_secs(30 * 86_400));
    let r = run_pass(&s, &p).await.unwrap();

    assert_eq!(r.sessions, 1, "only s-old's record goes");
    assert_eq!(r.waves, 1);
    assert_eq!(r.wave_tasks, 1);
    assert_eq!(
        r.envelopes_terminal, 3,
        "session.s-old + wave.w-old + session.w-old--t1 envelopes"
    );
    assert_eq!(envelope_count(&s).await, 1, "s-recent's history survives");
    assert!(s.get_session("s-recent").await.unwrap().is_some());
    assert!(s.get_session("s-old").await.unwrap().is_none());
    assert!(s.get_wave("w-old").await.unwrap().is_none());
    assert!(s.get_wave_task("w-old", "t1").await.unwrap().is_none());
}

#[tokio::test]
async fn drains_across_bounded_batches() {
    let s = setup().await;
    let old = Utc::now() - chrono::Duration::days(60);
    for i in 0..25 {
        s.store_envelope(&env_at("a", &format!("chan.{i}"), old))
            .await
            .unwrap();
    }

    let mut p = policy(); // batch_size = 10
    p.max_envelope_age = Some(Duration::from_secs(86_400));
    let r = run_pass(&s, &p).await.unwrap();

    assert_eq!(r.envelopes_age, 25, "loops bounded batches until drained");
    assert_eq!(envelope_count(&s).await, 0);
}

#[tokio::test]
async fn disabled_policy_deletes_nothing() {
    let s = setup().await;
    s.store_envelope(&env_at(
        "a",
        "chan.a",
        Utc::now() - chrono::Duration::days(365),
    ))
    .await
    .unwrap();
    let r = run_pass(&s, &RetentionPolicy::disabled()).await.unwrap();
    assert!(r.is_empty());
    assert_eq!(envelope_count(&s).await, 1);
}

#[tokio::test]
async fn session_filter_smoke() {
    // Sanity: closed session appears in list_sessions for protection().
    let s = setup().await;
    s.create_session(session("s1", "closed", Some(Utc::now())))
        .await
        .unwrap();
    let all = s.list_sessions(&SessionFilter::new()).await.unwrap();
    assert_eq!(all.len(), 1);
}
