//! Snapshot isolation, conflicts, scans, pruning and in-memory checkpoints.

use crate::kv::Versionstamp;

use super::*;

#[test]
fn commit_makes_writes_visible() {
    let store = MemStore::new();
    let mut tx = rw(&store);
    tx.set(b"a".to_vec(), b"1".to_vec()).unwrap();
    assert_eq!(
        tx.get(b"a").unwrap(),
        Some(b"1".to_vec()),
        "own writes are visible"
    );

    let mut other = rw(&store);
    assert_eq!(
        other.get(b"a").unwrap(),
        None,
        "uncommitted writes are invisible"
    );

    let vs = tx.commit().unwrap();
    assert_eq!(vs, Versionstamp(1));
    assert_eq!(
        other.get(b"a").unwrap(),
        None,
        "snapshot taken before the commit"
    );
    assert_eq!(rw(&store).get(b"a").unwrap(), Some(b"1".to_vec()));
}

#[test]
fn cancel_and_drop_discard_writes() {
    let store = MemStore::new();
    let mut tx = rw(&store);
    tx.set(b"a".to_vec(), b"1".to_vec()).unwrap();
    tx.cancel().unwrap();
    assert!(matches!(tx.get(b"a"), Err(AtlantisError::TxFinished)));
    {
        let mut dropped = rw(&store);
        dropped.set(b"b".to_vec(), b"2".to_vec()).unwrap();
    }
    let mut check = rw(&store);
    assert_eq!(check.get(b"a").unwrap(), None);
    assert_eq!(check.get(b"b").unwrap(), None);
}

#[test]
fn write_write_conflict_first_committer_wins() {
    let store = MemStore::new();
    let mut a = rw(&store);
    let mut b = rw(&store);
    a.set(b"k".to_vec(), b"a".to_vec()).unwrap();
    b.set(b"k".to_vec(), b"b".to_vec()).unwrap();
    a.commit().unwrap();
    let err = b.commit().unwrap_err();
    assert!(err.is_retryable(), "{err}");
    assert_eq!(rw(&store).get(b"k").unwrap(), Some(b"a".to_vec()));
}

#[test]
fn snapshot_allows_write_skew_but_serializable_does_not() {
    for (isolation, expect_conflict) in [
        (Isolation::Snapshot, false),
        (Isolation::Serializable, true),
    ] {
        let store = MemStore::new();
        let mut setup = rw(&store);
        setup.set(b"x".to_vec(), b"1".to_vec()).unwrap();
        setup.set(b"y".to_vec(), b"1".to_vec()).unwrap();
        setup.commit().unwrap();

        let mut a = store.begin(TxMode::ReadWrite, isolation).unwrap();
        let mut b = store.begin(TxMode::ReadWrite, isolation).unwrap();
        a.get(b"y").unwrap();
        a.set(b"x".to_vec(), b"0".to_vec()).unwrap();
        b.get(b"x").unwrap();
        b.set(b"y".to_vec(), b"0".to_vec()).unwrap();
        a.commit().unwrap();
        assert_eq!(b.commit().is_err(), expect_conflict, "{isolation:?}");
    }
}

#[test]
fn serializable_detects_phantoms_in_scanned_ranges() {
    let store = MemStore::new();
    let mut a = store
        .begin(TxMode::ReadWrite, Isolation::Serializable)
        .unwrap();
    let mut b = rw(&store);
    assert!(a.scan(&range(b"p/", b"p0"), None).unwrap().is_empty());
    a.set(b"count".to_vec(), b"0".to_vec()).unwrap();
    b.set(b"p/new".to_vec(), b"1".to_vec()).unwrap();
    b.commit().unwrap();
    assert!(matches!(a.commit(), Err(AtlantisError::TxConflict(_))));
}

#[test]
fn read_only_transactions_cannot_write() {
    let store = MemStore::new();
    let mut tx = store.begin(TxMode::ReadOnly, Isolation::Snapshot).unwrap();
    assert!(matches!(
        tx.set(b"a".to_vec(), vec![]),
        Err(AtlantisError::TxReadOnly)
    ));
    assert!(matches!(tx.delete(b"a"), Err(AtlantisError::TxReadOnly)));
}

#[test]
fn scan_merges_local_writes_and_respects_limit() {
    let store = MemStore::new();
    let mut setup = rw(&store);
    for k in [b"a1", b"a2", b"a3", b"b1"] {
        setup.set(k.to_vec(), k.to_vec()).unwrap();
    }
    setup.commit().unwrap();

    let mut tx = rw(&store);
    tx.delete(b"a2").unwrap();
    tx.set(b"a0".to_vec(), b"new".to_vec()).unwrap();
    let keys: Vec<Vec<u8>> = tx
        .scan(&range(b"a", b"b"), None)
        .unwrap()
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    assert_eq!(keys, vec![b"a0".to_vec(), b"a1".to_vec(), b"a3".to_vec()]);
    assert_eq!(tx.scan(&range(b"a", b"b"), Some(2)).unwrap().len(), 2);
}

#[test]
fn old_versions_are_pruned_when_no_snapshot_needs_them() {
    let store = MemStore::new();
    for i in 0..10u8 {
        let mut tx = rw(&store);
        tx.set(b"k".to_vec(), vec![i]).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(store.version_count().unwrap(), 1);

    let mut reader = rw(&store); // pins the current version
    for i in 10..13u8 {
        let mut tx = rw(&store);
        tx.set(b"k".to_vec(), vec![i]).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(
        reader.get(b"k").unwrap(),
        Some(vec![9]),
        "pinned snapshot still readable"
    );
    assert!(store.version_count().unwrap() > 1);
    drop(reader);
    store.gc().unwrap();
    assert_eq!(store.version_count().unwrap(), 1);

    let mut tx = rw(&store);
    tx.delete(b"k").unwrap();
    tx.commit().unwrap();
    assert_eq!(
        store.key_count().unwrap(),
        0,
        "deleted keys disappear once unreachable"
    );
}

#[test]
fn checkpoint_roundtrip() {
    let dir = std::env::temp_dir().join(format!("atlantis-kv-test-{}", atlantis_core::Ulid::new()));
    let path = dir.join("records.kv");
    let store = MemStore::new();
    let mut tx = rw(&store);
    tx.set(b"a".to_vec(), b"1".to_vec()).unwrap();
    tx.set(b"b".to_vec(), b"2".to_vec()).unwrap();
    tx.commit().unwrap();
    let mut pending = rw(&store);
    pending.set(b"c".to_vec(), b"uncommitted".to_vec()).unwrap();
    store.checkpoint(&path).unwrap();

    let loaded = MemStore::open_or_create(&path).unwrap();
    let mut tx = rw(&loaded);
    assert_eq!(tx.get(b"a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(tx.get(b"c").unwrap(), None);
    assert_eq!(
        loaded.current_version().unwrap(),
        store.current_version().unwrap()
    );
    fs::remove_dir_all(&dir).unwrap();

    assert!(
        MemStore::open_or_create(&dir.join("missing.kv"))
            .unwrap()
            .key_count()
            .unwrap()
            == 0
    );
}

#[test]
fn watched_ranges_are_validated_under_snapshot_isolation() {
    use crate::keys::KeyRange;
    let store = MemStore::new();
    // `rw` opens snapshot-isolated transactions, which do not validate reads.
    let mut watcher = rw(&store);
    let mut bystander = rw(&store);
    watcher
        .watch(KeyRange {
            start: b"s".to_vec(),
            end: b"t".to_vec(),
        })
        .unwrap();
    bystander.get(b"s1").unwrap();
    watcher.set(b"own".to_vec(), b"1".to_vec()).unwrap();
    bystander.set(b"other".to_vec(), b"1".to_vec()).unwrap();

    let mut writer = rw(&store);
    writer.set(b"s1".to_vec(), b"x".to_vec()).unwrap();
    writer.commit().unwrap();

    assert!(matches!(
        watcher.commit(),
        Err(AtlantisError::TxConflict(_))
    ));
    // An unwatched read of the same key is not validated.
    bystander.commit().unwrap();

    // A change outside the range does not conflict.
    let mut watcher = rw(&store);
    watcher
        .watch(KeyRange {
            start: b"s".to_vec(),
            end: b"t".to_vec(),
        })
        .unwrap();
    watcher.set(b"own".to_vec(), b"2".to_vec()).unwrap();
    let mut writer = rw(&store);
    writer.set(b"u".to_vec(), b"x".to_vec()).unwrap();
    writer.commit().unwrap();
    watcher.commit().unwrap();
}
