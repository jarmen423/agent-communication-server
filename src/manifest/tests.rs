//! Unit tests for the manifest cache and its write rule (no NATS, no database).

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use futures_util::future::join_all;

use super::test_store::RowStore;
use super::*;
use crate::storage::{ManifestRow, Storage};

fn rec(identity: &str, rev: i64) -> ManifestRecord {
    ManifestRecord {
        identity: identity.into(),
        owner_kind: OwnerKind::User,
        user: Some("alice".into()),
        user_id: None,
        device: None,
        device_id: None,
        kind: IdentityKind::Device,
        agent: None,
        role: Role::Worker,
        accept: None,
        rev,
    }
}

fn with_device(mut record: ManifestRecord, device: &str) -> ManifestRecord {
    record.device = Some(device.into());
    record
}

fn writer_manifest() -> IdentityManifest {
    IdentityManifest::new(BTreeSet::from(["writer-1".to_string()]))
}

fn stored_rev(m: &IdentityManifest, id: &str) -> i64 {
    match m.lookup(id) {
        Lookup::Live(r) => r.rev,
        Lookup::Removed { rev } => rev,
        Lookup::Unknown => panic!("{id} is unknown"),
    }
}

#[track_caller]
fn assert_refused<T: std::fmt::Debug>(result: Result<T>, why: &str) {
    let err = result.expect_err("write should be refused");
    let refused = err
        .downcast_ref::<ManifestRefused>()
        .unwrap_or_else(|| panic!("not a refusal: {err:#}"));
    assert!(refused.0.contains(why), "got: {}", refused.0);
}

#[tokio::test]
async fn newer_rev_applies_and_older_is_stale() {
    let m = IdentityManifest::default();
    assert_eq!(m.put(None, rec("a", 5)).await.unwrap(), Apply::Applied);
    assert_eq!(m.put(None, rec("a", 7)).await.unwrap(), Apply::Applied);
    assert_eq!(
        m.put(None, rec("a", 6)).await.unwrap(),
        Apply::Stale { current_rev: 7 }
    );
    assert_eq!(stored_rev(&m, "a"), 7);
}

#[tokio::test]
async fn equal_rev_is_stale_whether_identical_or_not() {
    let m = IdentityManifest::default();
    m.put(None, rec("a", 5)).await.unwrap();
    assert_eq!(
        m.put(None, rec("a", 5)).await.unwrap(),
        Apply::Stale { current_rev: 5 }
    );
    let changed = with_device(rec("a", 5), "other");
    assert_eq!(
        m.put(None, changed).await.unwrap(),
        Apply::Stale { current_rev: 5 }
    );
    assert_eq!(m.get("a").unwrap().device, None, "first writer wins");
}

#[tokio::test]
async fn tombstone_blocks_older_and_equal_rev_puts() {
    let m = IdentityManifest::default();
    m.put(None, rec("a", 5)).await.unwrap();
    assert_eq!(m.remove(None, "a", 8).await.unwrap(), Apply::Applied);
    assert!(m.get("a").is_none());
    for rev in [7, 8] {
        assert_eq!(
            m.put(None, rec("a", rev)).await.unwrap(),
            Apply::Stale { current_rev: 8 }
        );
    }
    assert_eq!(m.lookup("a"), Lookup::Removed { rev: 8 });
    assert_eq!(m.put(None, rec("a", 9)).await.unwrap(), Apply::Applied);
    assert!(m.get("a").is_some());
}

#[tokio::test]
async fn remove_at_or_below_the_stored_rev_is_stale() {
    let m = IdentityManifest::default();
    m.put(None, rec("a", 5)).await.unwrap();
    for rev in [4, 5] {
        assert_eq!(
            m.remove(None, "a", rev).await.unwrap(),
            Apply::Stale { current_rev: 5 }
        );
    }
    assert!(m.get("a").is_some());
    m.remove(None, "a", 6).await.unwrap();
    assert_eq!(
        m.remove(None, "a", 6).await.unwrap(),
        Apply::Stale { current_rev: 6 }
    );
}

#[tokio::test]
async fn remove_of_an_unknown_identity_creates_a_tombstone() {
    let m = IdentityManifest::default();
    assert_eq!(m.lookup("new"), Lookup::Unknown);
    assert_eq!(m.remove(None, "new", 4).await.unwrap(), Apply::Applied);
    assert_eq!(m.lookup("new"), Lookup::Removed { rev: 4 });
    assert_eq!(
        m.put(None, rec("new", 3)).await.unwrap(),
        Apply::Stale { current_rev: 4 }
    );
    assert_eq!((m.live_count(), m.removed_count()), (0, 1));
}

#[tokio::test]
async fn reserved_and_writer_identities_are_refused() {
    let m = writer_manifest();
    for (id, why) in [
        ("hub-orchestrator", "hub component"),
        ("hub-server", "hub component"),
        ("writer-1", "manifest writer"),
    ] {
        assert_refused(m.put(None, rec(id, 1)).await, why);
        assert_refused(m.remove(None, id, 1).await, why);
        assert_refused(m.sync(None, 1, vec![rec(id, 1)]).await, why);
        assert_eq!(m.lookup(id), Lookup::Unknown);
    }
}

#[tokio::test]
async fn owner_identity_is_accepted() {
    let m = writer_manifest();
    assert_eq!(m.put(None, rec("owner", 1)).await.unwrap(), Apply::Applied);
}

#[tokio::test]
async fn invalid_record_and_negative_rev_are_refused() {
    let m = IdentityManifest::default();
    let mut bad = rec("a", 1);
    bad.user = None;
    assert_refused(m.put(None, bad).await, "user:");
    assert_refused(m.remove(None, "a", -1).await, "rev:");
    assert_refused(m.remove(None, "a.b", 1).await, "identity:");
    assert_refused(m.sync(None, -1, vec![]).await, "rev:");
    assert_eq!(m.lookup("a"), Lookup::Unknown);
}

#[tokio::test]
async fn sync_removes_absent_slots_below_the_snapshot_and_keeps_newer_ones() {
    let m = IdentityManifest::default();
    m.put(None, rec("stays", 3)).await.unwrap();
    m.put(None, rec("gone", 4)).await.unwrap();
    m.put(None, rec("newer", 20)).await.unwrap();
    m.put(None, rec("at-snapshot", 10)).await.unwrap();
    m.put(None, rec("ahead", 15)).await.unwrap();
    m.remove(None, "old-tombstone", 2).await.unwrap();

    let report = m
        .sync(
            None,
            10,
            vec![rec("stays", 9), rec("ahead", 10), rec("fresh", 8)],
        )
        .await
        .unwrap();
    assert_eq!(
        report,
        SyncReport {
            applied: 2, // stays, fresh
            stale: 1,   // ahead
            removed: 1, // gone
            live: 5,    // stays, fresh, ahead, newer, at-snapshot
        }
    );
    assert_eq!(stored_rev(&m, "stays"), 9);
    assert_eq!(m.lookup("gone"), Lookup::Removed { rev: 10 });
    assert_eq!(stored_rev(&m, "newer"), 20);
    assert_eq!(stored_rev(&m, "at-snapshot"), 10);
    assert_eq!(stored_rev(&m, "ahead"), 15);
    assert_eq!(
        m.lookup("old-tombstone"),
        Lookup::Removed { rev: 2 },
        "existing tombstones stay as they are"
    );
}

#[tokio::test]
async fn sync_does_not_revive_a_newer_tombstone() {
    let m = IdentityManifest::default();
    m.remove(None, "a", 8).await.unwrap();
    let report = m.sync(None, 10, vec![rec("a", 7)]).await.unwrap();
    assert_eq!((report.stale, report.live), (1, 0));
    assert_eq!(m.lookup("a"), Lookup::Removed { rev: 8 });
}

#[tokio::test]
async fn a_bad_snapshot_changes_nothing() {
    let store = RowStore::default();
    let storage: Option<&dyn Storage> = Some(&store);
    let m = writer_manifest();
    m.put(storage, rec("keep", 1)).await.unwrap();
    let before = store.rows.lock().unwrap().clone();

    let mut bad_field = rec("b", 2);
    bad_field.device = Some(String::new());
    for (records, why) in [
        (vec![rec("a", 2), rec("over", 11)], "above the snapshot"),
        (vec![rec("a", 2), rec("a", 3)], "duplicate"),
        (vec![rec("a", 2), rec("writer-1", 2)], "manifest writer"),
        (vec![rec("a", 2), bad_field], "device:"),
    ] {
        assert_refused(m.sync(storage, 10, records).await, why);
        assert_eq!(m.lookup("a"), Lookup::Unknown);
        assert_eq!(stored_rev(&m, "keep"), 1);
        assert_eq!(*store.rows.lock().unwrap(), before);
    }
}

#[tokio::test]
async fn sync_size_cap() {
    let m = IdentityManifest::default();
    let over: Vec<_> = (0..=MAX_SYNC_RECORDS)
        .map(|i| rec(&format!("a{i}"), 1))
        .collect();
    assert_refused(m.sync(None, 1, over).await, "records:");
}

#[tokio::test]
async fn lookup_and_writer_helpers() {
    let m = writer_manifest();
    assert!(m.writer_configured());
    assert!(m.is_writer("writer-1"));
    assert!(!m.is_writer("a"));
    assert!(!IdentityManifest::default().writer_configured());

    m.put(None, rec("a", 1)).await.unwrap();
    assert!(matches!(m.lookup("a"), Lookup::Live(r) if r.rev == 1));
    m.remove(None, "a", 2).await.unwrap();
    assert_eq!(m.lookup("a"), Lookup::Removed { rev: 2 });
    assert!(m.get("a").is_none());
    assert_eq!(m.lookup("b"), Lookup::Unknown);
}

#[tokio::test]
async fn list_pages_live_records_in_order() {
    let m = IdentityManifest::default();
    for id in ["c", "a", "d", "b"] {
        m.put(None, rec(id, 1)).await.unwrap();
    }
    m.remove(None, "b", 2).await.unwrap();

    let ids = |page: &[Arc<ManifestRecord>]| {
        page.iter()
            .map(|r| r.identity.clone())
            .collect::<Vec<String>>()
    };
    let (page, next) = m.list(None, 2);
    assert_eq!(ids(&page), ["a", "c"]);
    assert_eq!(next.as_deref(), Some("c"));
    let (page, next) = m.list(next.as_deref(), 2);
    assert_eq!(ids(&page), ["d"]);
    assert_eq!(next, None);
}

#[tokio::test]
async fn failing_storage_leaves_the_cache_unchanged() {
    let store = RowStore::default();
    let storage: Option<&dyn Storage> = Some(&store);
    let m = IdentityManifest::default();
    m.put(storage, rec("a", 1)).await.unwrap();
    store.fail.store(true, Ordering::SeqCst);

    let err = m.put(storage, rec("a", 2)).await.unwrap_err();
    assert!(err.downcast_ref::<ManifestRefused>().is_none());
    assert_eq!(stored_rev(&m, "a"), 1);

    assert!(m.remove(storage, "a", 2).await.is_err());
    assert!(m.get("a").is_some());

    assert!(m.sync(storage, 5, vec![rec("b", 5)]).await.is_err());
    assert_eq!(stored_rev(&m, "a"), 1);
    assert_eq!(m.lookup("b"), Lookup::Unknown);

    store.fail.store(false, Ordering::SeqCst);
    assert_eq!(m.put(storage, rec("a", 2)).await.unwrap(), Apply::Applied);
}

#[tokio::test]
async fn load_restores_live_slots_and_tombstones() {
    let store = RowStore::default();
    let storage: Option<&dyn Storage> = Some(&store);
    let m = IdentityManifest::default();
    m.put(storage, rec("a", 1)).await.unwrap();
    m.put(storage, rec("b", 2)).await.unwrap();
    m.remove(storage, "b", 3).await.unwrap();

    let fresh = IdentityManifest::default();
    assert_eq!(fresh.load(&store).await.unwrap(), 1);
    assert_eq!(stored_rev(&fresh, "a"), 1);
    assert_eq!(fresh.lookup("b"), Lookup::Removed { rev: 3 });
    assert_eq!(
        fresh.put(None, rec("b", 3)).await.unwrap(),
        Apply::Stale { current_rev: 3 },
        "a tombstone survives a restart"
    );
}

#[tokio::test]
async fn load_errors_reach_the_caller() {
    let store = RowStore::default();
    store.fail.store(true, Ordering::SeqCst);
    assert!(IdentityManifest::default().load(&store).await.is_err());

    store.fail.store(false, Ordering::SeqCst);
    store.rows.lock().unwrap().push(ManifestRow {
        identity: "a".into(),
        rev: 1,
        record: Some(rec("b", 1)),
    });
    assert!(IdentityManifest::default().load(&store).await.is_err());
}

#[tokio::test]
async fn load_ignores_rows_for_names_that_became_writers() {
    let store = RowStore::default();
    IdentityManifest::default()
        .put(Some(&store), rec("writer-1", 1))
        .await
        .unwrap();
    let m = writer_manifest();
    assert_eq!(m.load(&store).await.unwrap(), 0);
    assert_eq!(m.lookup("writer-1"), Lookup::Unknown);
}

#[tokio::test]
async fn concurrent_puts_end_at_the_highest_rev() {
    let store = RowStore::default();
    let m = IdentityManifest::default();
    // A fixed shuffle of 1..=50.
    let revs = (0..50).map(|i| (i * 37) % 50 + 1);
    let results = join_all(revs.map(|rev| m.put(Some(&store), rec("a", rev)))).await;
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(stored_rev(&m, "a"), 50);
    assert_eq!(store.rows.lock().unwrap()[0].rev, 50, "storage agrees");
}

#[test]
fn debug_prints_counts_only() {
    let m = writer_manifest();
    let text = format!("{m:?}");
    assert!(text.contains("writers: 1"), "{text}");
    assert!(!text.contains("writer-1"), "{text}");
}
