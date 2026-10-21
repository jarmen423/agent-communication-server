// Unit tests: the per-statement batch bound and edge cascade.
use super::*;

// The batch bound itself: one call to `delete_batch_ordered` must
// delete at most `batch` rows, even when more rows match.
use super::*;
use crate::protocol::{Envelope, MessageKind};
use crate::storage::SurrealStorage;

#[tokio::test]
async fn delete_batch_respects_bound() {
    let storage = SurrealStorage::connect_memory().await.unwrap();
    storage.migrate().await.unwrap();
    for i in 0..25 {
        let e = Envelope::new(
            "a",
            format!("chan.{i}"),
            MessageKind::Message,
            serde_json::json!({}),
        );
        storage.store_envelope(&e).await.unwrap();
    }
    let db = storage.db();
    let n = delete_batch_ordered(
        &db,
        "envelopes",
        "SELECT id, timestamp FROM envelopes ORDER BY timestamp ASC LIMIT $batch",
        &[],
        10,
    )
    .await
    .unwrap();
    assert_eq!(n, 10);
    assert_eq!(count_envelopes(&db).await.unwrap(), 15);
    // Deleting envelopes also removes their reply_to edges.
}

#[tokio::test]
async fn deleted_envelopes_drop_reply_edges() {
    let storage = SurrealStorage::connect_memory().await.unwrap();
    storage.migrate().await.unwrap();
    let parent = Envelope::new("a", "c", MessageKind::Message, serde_json::json!({}));
    let reply = Envelope::new("b", "c", MessageKind::Message, serde_json::json!({}))
        .reply_to(&parent.meta.id);
    storage.store_envelope(&parent).await.unwrap();
    storage.store_envelope(&reply).await.unwrap();
    let db = storage.db();
    let edge_n = async {
        let rows: Vec<Value> = db
            .query("SELECT count() AS n FROM reply_to GROUP ALL")
            .await
            .unwrap()
            .check()
            .unwrap()
            .take(0)
            .unwrap();
        rows.first()
            .and_then(|r| r.get("n"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    assert_eq!(edge_n.await, 1);

    let n = delete_batch_ordered(
        &db,
        "envelopes",
        "SELECT id FROM envelopes LIMIT $batch",
        &[],
        10,
    )
    .await
    .unwrap();
    assert_eq!(n, 2);
    let edge_n = async {
        let rows: Vec<Value> = db
            .query("SELECT count() AS n FROM reply_to GROUP ALL")
            .await
            .unwrap()
            .check()
            .unwrap()
            .take(0)
            .unwrap();
        rows.first()
            .and_then(|r| r.get("n"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    assert_eq!(
        edge_n.await,
        0,
        "deleting envelopes must drop their reply_to edges"
    );
}
