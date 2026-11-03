//! Identity manifest: who owns each identity, as pushed to the hub by a
//! provisioning system (or `hub-admin`). The router reads it on the hot path
//! to stamp message provenance (`meta.origin`, see [`stamp`]).
//!
//! The wire record is [`ManifestRecord`] (Apache-2.0 client crate). This
//! module holds the server's cache and its write rule:
//!
//! - **Order.** A write whose `rev` is at or below the stored one changes
//!   nothing and answers [`Apply::Stale`], which is a success. Revs come from
//!   one increasing sequence, are comparable across identities and have gaps.
//! - **Removal.** A removal leaves a tombstone at its rev, kept for good, so a
//!   delayed older put can't bring a revoked identity back.
//! - **Full sync.** All-or-nothing: live slots below the snapshot rev that are
//!   missing from the snapshot are tombstoned at that rev.
//! - **Persistence.** Storage is written first, under one mutex, and the cache
//!   only afterwards. Only one process opens the database, so the two can't
//!   diverge, and a failed write leaves the cache as it was.
//!
//! **Why a sync `RwLock`:** `RouterCore::on_send` is a sync function on the
//! hot path. Reads clone an `Arc` and drop the guard at once, and the guard is
//! never held across an `.await`, so this is the one place the cache doesn't
//! use a tokio lock.
//!
//! This module has no database dependency and compiles without
//! `storage-surreal`. A hub without storage starts no query API, so nothing
//! can write its manifest and it stays empty: stamping then gives the tenant,
//! the bound identity and `reported`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use anyhow::{bail, Result};
use tracing::warn;

pub use nats_hub_client::manifest::*;

use crate::storage::{ManifestRow, Storage};

pub(crate) mod api;
pub(crate) mod stamp;
#[cfg(test)]
mod test_store;
#[cfg(test)]
mod tests;

/// A write refused for what it says, not because storage failed. Reach it
/// with `err.downcast_ref::<ManifestRefused>()`; the text reads
/// `"<field>: <why>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestRefused(pub String);

impl fmt::Display for ManifestRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ManifestRefused {}

#[derive(Clone)]
enum Slot {
    Live(Arc<ManifestRecord>),
    Removed { rev: i64 },
}

impl Slot {
    fn rev(&self) -> i64 {
        match self {
            Slot::Live(rec) => rec.rev,
            Slot::Removed { rev } => *rev,
        }
    }

    fn to_row(&self, identity: &str) -> ManifestRow {
        ManifestRow {
            identity: identity.to_string(),
            rev: self.rev(),
            record: match self {
                Slot::Live(rec) => Some(ManifestRecord::clone(rec)),
                Slot::Removed { .. } => None,
            },
        }
    }
}

/// Outcome of a put or remove. Both are successes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    Applied,
    /// The hub already holds this rev or a newer one; nothing changed.
    Stale {
        current_rev: i64,
    },
}

/// What the manifest knows about an identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Live(Arc<ManifestRecord>),
    Removed { rev: i64 },
    Unknown,
}

/// Outcome of a full sync.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Records from the snapshot that were written.
    pub applied: usize,
    /// Records from the snapshot the hub already held at this rev or newer.
    pub stale: usize,
    /// Live slots tombstoned because the snapshot no longer has them.
    pub removed: usize,
    /// Live identities after the sync.
    pub live: usize,
}

/// The hub's identity manifest: an in-memory cache over `Storage`.
pub struct IdentityManifest {
    /// `--manifest-writer` identities; fixed at construction.
    writers: BTreeSet<String>,
    /// Hot path: sync read, never held across `.await` (see the module docs).
    slots: RwLock<HashMap<String, Slot>>,
    /// Serializes put, remove, sync and load.
    write: tokio::sync::Mutex<()>,
}

impl Default for IdentityManifest {
    fn default() -> Self {
        Self::new(BTreeSet::new())
    }
}

impl fmt::Debug for IdentityManifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdentityManifest")
            .field("writers", &self.writers.len())
            .field("live", &self.live_count())
            .field("removed", &self.removed_count())
            .finish()
    }
}

impl IdentityManifest {
    /// An empty manifest that these identities may write.
    pub fn new(writers: BTreeSet<String>) -> Self {
        Self {
            writers,
            slots: RwLock::new(HashMap::new()),
            write: tokio::sync::Mutex::new(()),
        }
    }

    // A poisoned lock only means another thread panicked between two
    // `HashMap` calls that each leave the map valid, so keep routing.
    fn read_slots(&self) -> RwLockReadGuard<'_, HashMap<String, Slot>> {
        self.slots.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_slots(&self) -> RwLockWriteGuard<'_, HashMap<String, Slot>> {
        self.slots.write().unwrap_or_else(PoisonError::into_inner)
    }

    /// True when at least one manifest writer is configured.
    pub fn writer_configured(&self) -> bool {
        !self.writers.is_empty()
    }

    /// Is `identity` a configured manifest writer?
    pub fn is_writer(&self, identity: &str) -> bool {
        self.writers.contains(identity)
    }

    /// The configured manifest writers.
    pub fn writers(&self) -> &BTreeSet<String> {
        &self.writers
    }

    /// The live record for `identity`; a removed identity has none.
    pub fn get(&self, identity: &str) -> Option<Arc<ManifestRecord>> {
        match self.read_slots().get(identity) {
            Some(Slot::Live(rec)) => Some(rec.clone()),
            _ => None,
        }
    }

    /// What the manifest knows about `identity`.
    pub fn lookup(&self, identity: &str) -> Lookup {
        match self.read_slots().get(identity) {
            Some(Slot::Live(rec)) => Lookup::Live(rec.clone()),
            Some(Slot::Removed { rev }) => Lookup::Removed { rev: *rev },
            None => Lookup::Unknown,
        }
    }

    /// Number of live records.
    pub fn live_count(&self) -> usize {
        let slots = self.read_slots();
        slots
            .values()
            .filter(|s| matches!(s, Slot::Live(_)))
            .count()
    }

    /// Number of tombstones.
    pub fn removed_count(&self) -> usize {
        let slots = self.read_slots();
        slots
            .values()
            .filter(|s| matches!(s, Slot::Removed { .. }))
            .count()
    }

    /// One page of live records sorted by identity, starting after `after`,
    /// plus the cursor for the next page when there is one.
    pub fn list(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> (Vec<Arc<ManifestRecord>>, Option<String>) {
        let mut live: Vec<Arc<ManifestRecord>> = self
            .read_slots()
            .iter()
            .filter(|(id, _)| after.map_or(true, |a| id.as_str() > a))
            .filter_map(|(_, slot)| match slot {
                Slot::Live(rec) => Some(rec.clone()),
                Slot::Removed { .. } => None,
            })
            .collect();
        live.sort_by(|a, b| a.identity.cmp(&b.identity));
        let more = live.len() > limit;
        live.truncate(limit);
        let next = more
            .then(|| live.last().map(|rec| rec.identity.clone()))
            .flatten();
        (live, next)
    }

    /// Replace the cache with what storage holds; returns the live count. An
    /// error must stop the hub: routing on an empty manifest would silently
    /// lose provenance.
    pub async fn load(&self, storage: &dyn Storage) -> Result<usize> {
        let _write = self.write.lock().await;
        let rows = storage.load_identity_manifest().await?;
        let mut slots = HashMap::with_capacity(rows.len());
        for row in rows {
            // Records are refused for these names on the way in, so a row can
            // only be here if the hub's configuration changed since.
            if self.check_name(&row.identity).is_err() {
                warn!(identity = %row.identity, "ignoring stored manifest row for a reserved or writer identity");
                continue;
            }
            let slot = match row.record {
                Some(rec) if rec.identity != row.identity => {
                    bail!(
                        "stored manifest row '{}' holds a record for '{}'",
                        row.identity,
                        rec.identity
                    )
                }
                Some(rec) => Slot::Live(Arc::new(rec)),
                None => Slot::Removed { rev: row.rev },
            };
            slots.insert(row.identity, slot);
        }
        *self.write_slots() = slots;
        Ok(self.live_count())
    }

    /// Write one record (see the module docs for the rule).
    pub async fn put(&self, storage: Option<&dyn Storage>, rec: ManifestRecord) -> Result<Apply> {
        self.check_record(&rec)?;
        let _write = self.write.lock().await;
        if let Some(current_rev) = self.stale_against(&rec.identity, rec.rev) {
            return Ok(Apply::Stale { current_rev });
        }
        let identity = rec.identity.clone();
        self.persist_and_cache(storage, identity, Slot::Live(Arc::new(rec)))
            .await?;
        Ok(Apply::Applied)
    }

    /// Tombstone `identity` at `rev`, even if the hub has never heard of it,
    /// so a delayed older put can't create it later.
    pub async fn remove(
        &self,
        storage: Option<&dyn Storage>,
        identity: &str,
        rev: i64,
    ) -> Result<Apply> {
        self.check_name(identity)?;
        if rev < 0 {
            return Err(ManifestRefused("rev: must be at least 0".to_string()).into());
        }
        let _write = self.write.lock().await;
        if let Some(current_rev) = self.stale_against(identity, rev) {
            return Ok(Apply::Stale { current_rev });
        }
        self.persist_and_cache(storage, identity.to_string(), Slot::Removed { rev })
            .await?;
        Ok(Apply::Applied)
    }

    /// Apply a full snapshot taken at rev `rev`, all or nothing. Slots at
    /// `rev` or above were written after the snapshot and are kept.
    pub async fn sync(
        &self,
        storage: Option<&dyn Storage>,
        rev: i64,
        records: Vec<ManifestRecord>,
    ) -> Result<SyncReport> {
        self.check_snapshot(rev, &records)?;
        let _write = self.write.lock().await;

        let mut next = self.read_slots().clone();
        let mut report = SyncReport::default();
        let named: HashSet<String> = records.iter().map(|r| r.identity.clone()).collect();
        for rec in records {
            match next.get(&rec.identity) {
                Some(slot) if rec.rev <= slot.rev() => report.stale += 1,
                _ => {
                    report.applied += 1;
                    next.insert(rec.identity.clone(), Slot::Live(Arc::new(rec)));
                }
            }
        }
        for (identity, slot) in next.iter_mut() {
            if matches!(slot, Slot::Live(_)) && slot.rev() < rev && !named.contains(identity) {
                *slot = Slot::Removed { rev };
                report.removed += 1;
            }
        }
        report.live = next.values().filter(|s| matches!(s, Slot::Live(_))).count();

        if let Some(storage) = storage {
            let mut rows: Vec<ManifestRow> =
                next.iter().map(|(id, slot)| slot.to_row(id)).collect();
            rows.sort_by(|a, b| a.identity.cmp(&b.identity));
            storage.replace_identity_manifest(&rows).await?;
        }
        *self.write_slots() = next;
        Ok(report)
    }

    /// Reserved identities and manifest writers never get a slot.
    fn check_name(&self, identity: &str) -> Result<(), ManifestRefused> {
        check_identity(identity).map_err(ManifestRefused)?;
        if self.is_writer(identity) {
            return Err(ManifestRefused(
                "identity: reserved for the manifest writer".to_string(),
            ));
        }
        Ok(())
    }

    fn check_record(&self, rec: &ManifestRecord) -> Result<(), ManifestRefused> {
        rec.validate().map_err(ManifestRefused)?;
        self.check_name(&rec.identity)
    }

    fn check_snapshot(&self, rev: i64, records: &[ManifestRecord]) -> Result<(), ManifestRefused> {
        let refuse = |why: String| Err(ManifestRefused(why));
        if rev < 0 {
            return refuse("rev: must be at least 0".to_string());
        }
        if records.len() > MAX_SYNC_RECORDS {
            return refuse(format!("records: more than {MAX_SYNC_RECORDS}"));
        }
        let mut seen = HashSet::new();
        for rec in records {
            self.check_record(rec)?;
            if !seen.insert(rec.identity.as_str()) {
                return refuse(format!("identity: duplicate record for {}", rec.identity));
            }
            if rec.rev > rev {
                return refuse(format!(
                    "rev: record for {} is at {}, above the snapshot rev {rev}",
                    rec.identity, rec.rev
                ));
            }
        }
        Ok(())
    }

    /// The stored rev, when it means a write at `rev` must not apply.
    fn stale_against(&self, identity: &str, rev: i64) -> Option<i64> {
        self.read_slots()
            .get(identity)
            .map(Slot::rev)
            .filter(|current| rev <= *current)
    }

    async fn persist_and_cache(
        &self,
        storage: Option<&dyn Storage>,
        identity: String,
        slot: Slot,
    ) -> Result<()> {
        if let Some(storage) = storage {
            storage
                .save_identity_manifest_row(&slot.to_row(&identity))
                .await?;
        }
        self.write_slots().insert(identity, slot);
        Ok(())
    }
}
