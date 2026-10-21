//! Bounded batch deletion shared by every retention rule.
//!
//! SurrealQL `DELETE` has no `LIMIT`, so a batch is: `SELECT id …
//! LIMIT $batch` (id preselection), then `DELETE … WHERE id IN $ids`.
//! Loops yield between batches so the router mirror and query API
//! keep their latency even while a large backlog drains.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::storage::surreal::Db;

/// One bounded batch-delete returning the deleted count. `sel` is the full
/// `SELECT id …` statement (with `LIMIT $batch` inside); `table` is
/// where the ids get deleted from.
pub(crate) async fn delete_batch_ordered(
    db: &Db,
    table: &str,
    sel: &str,
    binds: &[(&'static str, Value)],
    batch: usize,
) -> Result<u64> {
    let sql = format!(
        "LET $rows = ({sel}); \
         LET $ids = array::map($rows, |$r| $r.id); \
         DELETE {table} WHERE id IN $ids; \
         RETURN array::len($ids);"
    );
    let mut q = db.query(sql).bind(("batch", batch as i64));
    for (k, v) in binds {
        q = q.bind((*k, v.clone()));
    }
    let n: Option<i64> = q.await?.check()?.take(3)?;
    Ok(n.unwrap_or(0).max(0) as u64)
}

/// Repeat [`delete_batch_ordered`] until the rule is satisfied, yielding
/// between batches so other DB users (router mirror, queries) get through.
pub(crate) async fn delete_loop(
    db: &Db,
    table: &str,
    sel: &str,
    binds: &[(&'static str, Value)],
    batch: usize,
) -> Result<u64> {
    let mut total = 0u64;
    loop {
        let n = delete_batch_ordered(db, table, sel, binds, batch)
            .await
            .with_context(|| format!("retention: delete from {table}"))?;
        total += n;
        if (n as usize) < batch {
            return Ok(total);
        }
        tokio::task::yield_now().await;
    }
}

/// Total stored envelopes (used by the size cap).
pub(crate) async fn count_envelopes(db: &Db) -> Result<u64> {
    let rows: Vec<Value> = db
        .query("SELECT count() AS n FROM envelopes GROUP ALL")
        .await?
        .check()?
        .take(0)?;
    Ok(rows
        .first()
        .and_then(|r| r.get("n"))
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(0) as u64)
}
