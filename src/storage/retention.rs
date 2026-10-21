//! Retention pruning for SurrealDB storage.
//!
//! hub-server runs [`spawn`] when any `RetentionPolicy` rule is configured.
//! Each pass deletes in bounded batches (`batch_size` rows per statement,
//! `LIMIT`-preselected ids) and yields between batches, so the router and
//! the storage mirror are never blocked. Deleting an envelope also drops
//! its `reply_to` edges in SurrealDB.
//!
//! Protection rule (the "keep live state" rule): a pass never deletes
//! - a session or wave record that is not terminal, and
//! - any envelope on a channel a live wave/session talks on:
//!   `session.<sid>`, `wave.<wid>`, `wave.<wid>.<…>`, and wave-task
//!   session channels `session.<wid>--<task>`.
//!
//! Terminal waves/sessions get their own (longer) retention
//! (`terminal_retention`): while a closed session or finished wave is
//! younger than that window its channel is also protected; once the record
//! ages out, the record and all of its channel's envelopes go together.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::analytics::metrics::MetricsCollector;
use crate::storage::surreal::Db;
use crate::storage::{SessionFilter, Storage, SurrealStorage};

/// Terminal statuses: session `status` is `closed`; a wave is finished in
/// `completed`/`failed`/`cancelled`; a wave task ends `done`/`failed`/`cancelled`.
const SESSION_TERMINAL: &str = "closed";
const WAVE_TERMINAL: &[&str] = &["completed", "failed", "cancelled"];

/// What gets pruned, and how fast.
#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    /// Delete envelopes older than this (except on protected channels).
    /// `None` = no age rule.
    pub max_envelope_age: Option<Duration>,
    /// Keep at most this many envelopes (protected rows are never deleted
    /// to satisfy the cap). `None` = no cap.
    pub max_envelopes: Option<u64>,
    /// How long terminal sessions/waves (records + their channels) are
    /// kept after closing. `None` = terminal state gets no special
    /// retention — records are never pruned and terminal channels fall
    /// under the general rules.
    pub terminal_retention: Option<Duration>,
    /// Rows deleted per DELETE statement.
    pub batch_size: usize,
    /// Time between passes.
    pub interval: Duration,
}

impl RetentionPolicy {
    /// Default batch bound per DELETE statement.
    pub const DEFAULT_BATCH: usize = 1000;
    /// Default pass interval (1h).
    pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(3600);

    /// Everything off.
    pub fn disabled() -> Self {
        Self {
            max_envelope_age: None,
            max_envelopes: None,
            terminal_retention: None,
            batch_size: Self::DEFAULT_BATCH,
            interval: Self::DEFAULT_INTERVAL,
        }
    }

    /// Any rule configured?
    pub fn enabled(&self) -> bool {
        self.max_envelope_age.is_some()
            || self.max_envelopes.is_some()
            || self.terminal_retention.is_some()
    }
}

/// What a pass deleted.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PruneReport {
    /// Envelopes pruned by the age rule on ordinary channels.
    pub envelopes_age: u64,
    /// Envelopes pruned on channels of aged-out terminal waves/sessions.
    pub envelopes_terminal: u64,
    /// Envelopes pruned by the size cap.
    pub envelopes_cap: u64,
    /// Session records pruned (terminal, past terminal retention).
    pub sessions: u64,
    /// Wave records pruned (terminal, past terminal retention).
    pub waves: u64,
    /// Wave-task records pruned.
    pub wave_tasks: u64,
}

impl PruneReport {
    /// Rows deleted in total.
    pub fn total(&self) -> u64 {
        self.envelopes_age
            + self.envelopes_terminal
            + self.envelopes_cap
            + self.sessions
            + self.waves
            + self.wave_tasks
    }

    /// Anything deleted?
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

/// Channels a pass must treat specially.
///
/// `live_*` is never deleted. `terminal_*` belongs to terminal records
/// still inside their retention window: protected from the general age
/// rule. `aged_terminal_*` belongs to terminal records past the window:
/// every envelope on those channels goes.
#[derive(Debug, Default)]
struct Protection {
    live_exact: Vec<String>,
    live_prefixes: Vec<String>,
    terminal_exact: Vec<String>,
    terminal_prefixes: Vec<String>,
    aged_terminal_exact: Vec<String>,
    aged_terminal_prefixes: Vec<String>,
}

/// Compute the protection sets from live storage state.
///
/// Channel conventions (`meta.channel`, not the NATS subject):
/// `session.<sid>` for a session, `wave.<wid>` (+ `wave.<wid>.<…>`) for a
/// wave, and `session.<wid>--<task>` for a wave task's session channel.
async fn protection(
    storage: &SurrealStorage,
    terminal_retention: Option<Duration>,
) -> Result<Protection> {
    let mut p = Protection::default();
    let tcut = terminal_retention
        .map(|d| Utc::now() - chrono::Duration::from_std(d).unwrap_or(chrono::Duration::zero()));
    // A terminal record whose close time is missing keeps its best-known
    // timestamp; undatable records age out by their creation/update time
    // rather than living forever.
    let aged = |close: Option<DateTime<Utc>>| tcut.is_some_and(|t| close.is_some_and(|c| c < t));

    for s in storage
        .list_sessions(&SessionFilter::new())
        .await
        .context("retention: list sessions")?
    {
        let chan = format!("session.{}", s.session_id);
        if s.status == SESSION_TERMINAL {
            let close = s.closed_at.or(Some(s.updated_at));
            if aged(close) {
                p.aged_terminal_exact.push(chan);
            } else {
                p.terminal_exact.push(chan);
            }
        } else {
            p.live_exact.push(chan);
        }
    }

    for w in storage
        .list_waves(None)
        .await
        .context("retention: list waves")?
    {
        let wave_chan = format!("wave.{}", w.wave_id);
        let wave_pre = format!("wave.{}.", w.wave_id);
        let sess_pre = format!("session.{}--", w.wave_id);
        if WAVE_TERMINAL.contains(&w.status.as_str()) {
            let close = w.closed_at.or(Some(w.created_at));
            if aged(close) {
                p.aged_terminal_exact.push(wave_chan);
                p.aged_terminal_prefixes.extend([wave_pre, sess_pre]);
            } else {
                p.terminal_exact.push(wave_chan);
                p.terminal_prefixes.extend([wave_pre, sess_pre]);
            }
        } else {
            p.live_exact.push(wave_chan);
            p.live_prefixes.extend([wave_pre, sess_pre]);
        }
    }
    Ok(p)
}

/// One bounded batch-delete returning the deleted count. `sel` is the full
/// `SELECT VALUE id …` statement (with `LIMIT $batch` inside); `table` is
/// where the ids get deleted from.
async fn delete_batch_ordered(
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
async fn delete_loop(
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
pub async fn count_envelopes(db: &Db) -> Result<u64> {
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

/// `NOT protected` predicates over `channel` (shared by the envelope rules).
fn channel_excluded(exact: &str, prefixes: &str) -> String {
    format!(
        "channel NOT IN {exact} \
         AND array::all({prefixes}, |$p| !string::starts_with(channel, $p))"
    )
}

/// `IS on these channels` predicate (for aged-terminal pruning).
fn channel_in(exact: &str, prefixes: &str) -> String {
    format!(
        "(channel IN {exact} \
          OR array::any({prefixes}, |$p| string::starts_with(channel, $p)))"
    )
}

/// Run one prune pass. Each rule deletes in `batch_size` statements and
/// yields between batches; live waves/sessions and their channels are
/// never touched.
pub async fn run_pass(storage: &SurrealStorage, policy: &RetentionPolicy) -> Result<PruneReport> {
    let batch = policy.batch_size.max(1);
    let db = storage.db();
    let prot = protection(storage, policy.terminal_retention).await?;
    let mut report = PruneReport::default();

    let j = |v: &Vec<String>| Value::from(v.clone());
    let live_exact = j(&prot.live_exact);
    let live_prefixes = j(&prot.live_prefixes);
    let term_exact = j(&prot.terminal_exact);
    let term_prefixes = j(&prot.terminal_prefixes);
    let aged_exact = j(&prot.aged_terminal_exact);
    let aged_prefixes = j(&prot.aged_terminal_prefixes);

    // ── Terminal records past their retention window ──────────
    if let Some(tr) = policy.terminal_retention {
        let tcut = (Utc::now()
            - chrono::Duration::from_std(tr).unwrap_or(chrono::Duration::zero()))
        .timestamp();
        let binds = [("tcut", Value::from(tcut))];

        report.sessions += delete_loop(
            &db,
            "sessions",
            "SELECT id FROM sessions \
             WHERE status = 'closed' AND closed_at IS NOT NONE AND closed_at < time::from::secs($tcut) \
             LIMIT $batch",
            &binds,
            batch,
        )
        .await?;
        report.waves += delete_loop(
            &db,
            "waves",
            "SELECT id FROM waves \
             WHERE status IN ['completed', 'failed', 'cancelled'] \
               AND closed_at IS NOT NONE AND closed_at < time::from::secs($tcut) \
             LIMIT $batch",
            &binds,
            batch,
        )
        .await?;
        report.wave_tasks += delete_loop(
            &db,
            "wave_tasks",
            "SELECT id FROM wave_tasks WITH NOINDEX \
             WHERE status IN ['done', 'failed', 'cancelled'] \
               AND completed_at IS NOT NONE AND completed_at < time::from::secs($tcut) \
               AND wave_id NOT IN (SELECT VALUE wave_id FROM waves \
                     WHERE status NOT IN ['completed', 'failed', 'cancelled']) \
             LIMIT $batch",
            &binds,
            batch,
        )
        .await?;
        // Envelopes on aged-out terminal channels go wholesale — the
        // session/wave retention window covers the whole history.
        if !prot.aged_terminal_exact.is_empty() || !prot.aged_terminal_prefixes.is_empty() {
            let sel = format!(
                "SELECT id FROM envelopes WITH NOINDEX WHERE {} LIMIT $batch",
                channel_in("$aged_exact", "$aged_prefixes")
            );
            report.envelopes_terminal += delete_loop(
                &db,
                "envelopes",
                &sel,
                &[
                    ("aged_exact", aged_exact.clone()),
                    ("aged_prefixes", aged_prefixes.clone()),
                ],
                batch,
            )
            .await?;
        }
    }

    // ── Age rule on ordinary channels ──────────────────────────
    if let Some(age) = policy.max_envelope_age {
        let cutoff = (Utc::now()
            - chrono::Duration::from_std(age).unwrap_or(chrono::Duration::zero()))
        .timestamp();
        let sel = format!(
            "SELECT id, timestamp FROM envelopes WITH NOINDEX \
             WHERE timestamp < time::from::secs($cutoff) AND {} AND {} \
             ORDER BY timestamp ASC LIMIT $batch",
            channel_excluded("$live_exact", "$live_prefixes"),
            channel_excluded("$term_exact", "$term_prefixes"),
        );
        report.envelopes_age += delete_loop(
            &db,
            "envelopes",
            &sel,
            &[
                ("cutoff", Value::from(cutoff)),
                ("live_exact", live_exact.clone()),
                ("live_prefixes", live_prefixes.clone()),
                ("term_exact", term_exact.clone()),
                ("term_prefixes", term_prefixes.clone()),
            ],
            batch,
        )
        .await?;
    }

    // ── Size cap: drop the oldest unprotected envelopes ────────
    if let Some(cap) = policy.max_envelopes {
        let sel = format!(
            "SELECT id, timestamp FROM envelopes WITH NOINDEX \
             WHERE {} ORDER BY timestamp ASC LIMIT $batch",
            channel_excluded("$live_exact", "$live_prefixes"),
        );
        loop {
            let total = count_envelopes(&db).await?;
            if total <= cap {
                break;
            }
            // Never delete more than we're over by — the cap keeps exactly
            // `cap` rows even when batch_size is larger.
            let want = (total - cap).min(batch as u64) as usize;
            let n = delete_batch_ordered(
                &db,
                "envelopes",
                &sel,
                &[
                    ("live_exact", live_exact.clone()),
                    ("live_prefixes", live_prefixes.clone()),
                ],
                want,
            )
            .await?;
            report.envelopes_cap += n;
            if n == 0 {
                break; // everything over the cap is protected — keep it
            }
            tokio::task::yield_now().await;
        }
    }

    debug!(?report, "retention pass done");
    Ok(report)
}

/// Spawn the background pruner. First pass runs immediately (a backlog
/// shouldn't sit for a whole interval); then every `policy.interval`.
/// Errors are counted on `/metrics` and logged; the loop keeps running.
pub fn spawn(
    storage: Arc<SurrealStorage>,
    policy: RetentionPolicy,
    metrics: Arc<MetricsCollector>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        info!(
            age = ?policy.max_envelope_age,
            max = ?policy.max_envelopes,
            terminal = ?policy.terminal_retention,
            batch = policy.batch_size,
            interval = ?policy.interval,
            "retention pruner running"
        );
        let mut ticker = tokio::time::interval(policy.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match run_pass(&storage, &policy).await {
                Ok(r) => {
                    if !r.is_empty() {
                        info!(report = ?r, "retention pass pruned rows");
                    }
                    metrics.record_pruned(r.total());
                }
                Err(e) => {
                    metrics.record_prune_error();
                    warn!(error = %format!("{e:#}"), "retention pass failed");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    //! The batch bound itself: one call to `delete_batch_ordered` must
    //! delete at most `batch` rows, even when more rows match.
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
}
