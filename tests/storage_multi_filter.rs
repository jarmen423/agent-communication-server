//! Regression: multi-condition history filters must match every condition.
//!
//! W2-B found that SurrealDB 2.6.5's planner can pick one index for a
//! multi-condition WHERE and skip the remaining conditions (seen on its
//! aggregate analytics queries, which now use `WITH NOINDEX`). `query_history`
//! (behind `history.query`) builds the same kind of WHERE, so every filter
//! combination is checked here against a brute-force filter, on both engines.

use chrono::{Duration, Utc};
use nats_hub::{Envelope, HistoryQuery, MessageKind, Storage, SurrealStorage};
use serde_json::json;

#[tokio::test]
async fn history_filters_match_every_condition_memory() {
    let storage = SurrealStorage::connect_memory().await.unwrap();
    check(&storage).await;
}

#[tokio::test]
async fn history_filters_match_every_condition_rocksdb() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let storage = SurrealStorage::connect(path.to_str().unwrap())
        .await
        .unwrap();
    check(&storage).await;
}

async fn check(storage: &SurrealStorage) {
    storage.migrate().await.unwrap();
    let start = Utc::now() - Duration::seconds(1);

    let mut all = Vec::new();
    for from in ["alice", "bob"] {
        for channel in ["ch-a", "ch-b"] {
            for kind in [MessageKind::Message, MessageKind::Event] {
                for to in [None, Some("carol"), Some("dave")] {
                    let mut env = Envelope::new(from, channel, kind.clone(), json!({}));
                    if let Some(to) = to {
                        env = env.to(to);
                    }
                    storage.store_envelope(&env).await.unwrap();
                    all.push(env);
                }
            }
        }
    }

    let kinds = [None, Some("message"), Some("event")];
    for channel in [None, Some("ch-a")] {
        for from in [None, Some("alice")] {
            for to in [None, Some("carol")] {
                for kind in kinds {
                    for since in [None, Some(start)] {
                        let mut q = HistoryQuery::new().limit(1000);
                        q.channel = channel.map(String::from);
                        q.from = from.map(String::from);
                        q.to = to.map(String::from);
                        q.kind = kind.map(String::from);
                        q.since = since;
                        let got = storage.query_history(&q).await.unwrap();
                        let want = all
                            .iter()
                            .filter(|e| channel.is_none_or(|c| e.meta.channel == c))
                            .filter(|e| from.is_none_or(|f| e.meta.from == f))
                            .filter(|e| to.is_none_or(|t| e.meta.to.as_deref() == Some(t)))
                            .filter(|e| {
                                kind.is_none_or(|k| {
                                    serde_json::to_value(&e.meta.kind).unwrap() == k
                                })
                            })
                            .count();
                        for r in &got {
                            assert!(channel.is_none_or(|c| r.channel == c), "{q:?}: {r:?}");
                            assert!(from.is_none_or(|f| r.from_identity == f), "{q:?}: {r:?}");
                            assert!(
                                to.is_none_or(|t| r.to_identity.as_deref() == Some(t)),
                                "{q:?}: {r:?}"
                            );
                            assert!(kind.is_none_or(|k| r.kind == k), "{q:?}: {r:?}");
                        }
                        assert_eq!(got.len(), want, "{q:?}");
                    }
                }
            }
        }
    }
}
