//! The commit path: what reaches the log, failed appends, staging and idempotent writes.

use super::*;

#[test]
fn aborted_and_conflicting_transactions_never_reach_the_log() {
    let dir = TempDir::new();
    let store = open(&dir);
    let mut a = rw(&store);
    let mut b = rw(&store);
    a.set(b"k".to_vec(), b"a".to_vec()).unwrap();
    b.set(b"k".to_vec(), b"b".to_vec()).unwrap();
    a.commit().unwrap();
    assert!(b.commit().is_err());

    let mut cancelled = rw(&store);
    cancelled.set(b"x".to_vec(), b"1".to_vec()).unwrap();
    cancelled.cancel().unwrap();
    {
        let mut dropped = rw(&store);
        dropped.set(b"y".to_vec(), b"1".to_vec()).unwrap();
    }
    let mut empty = rw(&store);
    empty.get(b"k").unwrap();
    empty.commit().unwrap();
    let mut ro = store.begin(TxMode::ReadOnly, Isolation::Snapshot).unwrap();
    ro.get(b"k").unwrap();
    ro.commit().unwrap();

    put(&store, b"z", b"1");
    assert_eq!(logged_versions(&dir.file(wal::WAL_FILE)), vec![1, 2]);
    drop(store);
    let store = open(&dir);
    assert_eq!(get(&store, b"k"), Some(b"a".to_vec()));
    assert_eq!(get(&store, b"x"), None);
    assert_eq!(get(&store, b"y"), None);
}

#[test]
fn failed_log_append_fails_the_commit_and_applies_nothing() {
    let dir = TempDir::new();
    let store = open(&dir);
    put(&store, b"a", b"1");
    {
        // Swap in a read-only handle so the next append fails.
        let read_only = fs::File::open(dir.file(wal::WAL_FILE)).unwrap();
        let mut inner = store.write().unwrap();
        inner.wal.as_mut().unwrap().replace_file_for_test(read_only);
    }
    let mut tx = rw(&store);
    tx.set(b"a".to_vec(), b"2".to_vec()).unwrap();
    tx.set(b"b".to_vec(), b"2".to_vec()).unwrap();
    assert!(matches!(tx.commit(), Err(AtlantisError::Storage(_))));
    assert!(tx.is_finished());
    assert_eq!(version(&store), 1);
    assert_eq!(get(&store, b"a"), Some(b"1".to_vec()));
    assert_eq!(get(&store, b"b"), None);
    assert!(store.read().unwrap().active.is_empty(), "snapshot released");
    drop(tx);
    drop(store);

    let store = open(&dir);
    assert_eq!(version(&store), 1);
    assert_eq!(get(&store, b"b"), None);
    assert_eq!(put(&store, b"b", b"3"), 2);
}

#[test]
fn staged_commits_are_invisible_and_conflict_until_published() {
    let dir = TempDir::new();
    let store = open(&dir);
    put(&store, b"other", b"0");
    let mut t1 = store
        .begin_tx(TxMode::ReadWrite, Isolation::Snapshot)
        .unwrap();
    t1.set(b"k".to_vec(), b"1".to_vec()).unwrap();
    let (v, wait) = t1.stage(&store).unwrap();
    assert!(wait, "SyncMode::Every waits for the sync");
    assert_eq!(v, 2);

    // Not visible yet: readers see version 1.
    assert_eq!(version(&store), 1);
    assert_eq!(get(&store, b"k"), None);

    // A writer of the same key loses, although the key is not visible.
    let mut t2 = rw(&store);
    t2.set(b"k".to_vec(), b"2".to_vec()).unwrap();
    assert!(matches!(t2.commit(), Err(AtlantisError::TxConflict(_))));

    // A serializable transaction that read the key, or scanned over it,
    // loses too.
    let mut t3 = store
        .begin(TxMode::ReadWrite, Isolation::Serializable)
        .unwrap();
    assert_eq!(t3.get(b"k").unwrap(), None);
    t3.set(b"x".to_vec(), b"1".to_vec()).unwrap();
    assert!(matches!(t3.commit(), Err(AtlantisError::TxConflict(_))));
    let mut t4 = store
        .begin(TxMode::ReadWrite, Isolation::Serializable)
        .unwrap();
    t4.scan(&range(b"a", b"z"), None).unwrap();
    t4.set(b"y".to_vec(), b"1".to_vec()).unwrap();
    assert!(matches!(t4.commit(), Err(AtlantisError::TxConflict(_))));

    // Another key commits behind it, and both become visible in order.
    let v_other = put(&store, b"z", b"9");
    assert_eq!(v_other, 3);
    assert_eq!(get(&store, b"k"), Some(b"1".to_vec()));
    assert_eq!(get(&store, b"z"), Some(b"9".to_vec()));
    assert_eq!(version(&store), 3);
    assert!(store.read().unwrap().pending.is_empty());
    assert!(store.read().unwrap().pending_keys.is_empty());
}

#[test]
fn idempotent_writes_of_the_same_value_do_not_conflict() {
    let dir = TempDir::new();
    let store = open(&dir);
    // t1 stages the catalog entry; t2 began before and writes the same
    // bytes, plus its own record.
    let mut t1 = store
        .begin_tx(TxMode::ReadWrite, Isolation::Snapshot)
        .unwrap();
    let mut t2 = store
        .begin_tx(TxMode::ReadWrite, Isolation::Serializable)
        .unwrap();
    let mut t3 = store
        .begin_tx(TxMode::ReadWrite, Isolation::Snapshot)
        .unwrap();
    t1.set_idempotent(b"cat".to_vec(), b"def".to_vec()).unwrap();
    t1.set(b"r1".to_vec(), b"1".to_vec()).unwrap();
    t1.stage(&store).unwrap();

    assert_eq!(t2.get(b"cat").unwrap(), None);
    t2.set_idempotent(b"cat".to_vec(), b"def".to_vec()).unwrap();
    t2.set(b"r2".to_vec(), b"2".to_vec()).unwrap();
    let v2 = t2.commit().unwrap();
    assert_eq!(
        v2.0, 2,
        "staged behind t1, and logged without the catalog key"
    );

    // A different value still conflicts.
    t3.set_idempotent(b"cat".to_vec(), b"other".to_vec())
        .unwrap();
    assert!(matches!(t3.commit(), Err(AtlantisError::TxConflict(_))));

    // A transaction whose only write is redundant commits as a no-op.
    let mut t4 = store
        .begin_tx(TxMode::ReadWrite, Isolation::Snapshot)
        .unwrap();
    let mut t5 = store
        .begin_tx(TxMode::ReadWrite, Isolation::Snapshot)
        .unwrap();
    t4.set_idempotent(b"x".to_vec(), b"1".to_vec()).unwrap();
    t4.commit().unwrap();
    t5.set_idempotent(b"x".to_vec(), b"1".to_vec()).unwrap();
    assert_eq!(t5.commit().unwrap().0, 2, "returns its snapshot");

    assert_eq!(get(&store, b"cat"), Some(b"def".to_vec()));
    assert_eq!(get(&store, b"r1"), Some(b"1".to_vec()));
    assert_eq!(get(&store, b"r2"), Some(b"2".to_vec()));
    assert_eq!(version(&store), 3);
    drop(store);
    let store = open(&dir);
    assert_eq!(get(&store, b"r2"), Some(b"2".to_vec()));
    assert_eq!(version(&store), 3);
}

#[test]
fn checkpoint_waits_for_staged_commits() {
    let dir = TempDir::new();
    let store = open(&dir);
    let mut t1 = store
        .begin_tx(TxMode::ReadWrite, Isolation::Snapshot)
        .unwrap();
    t1.set(b"k".to_vec(), b"1".to_vec()).unwrap();
    t1.stage(&store).unwrap();
    store.checkpoint_durable().unwrap();
    assert_eq!(get(&store, b"k"), Some(b"1".to_vec()));
    drop(store);
    let store = open(&dir);
    assert_eq!(get(&store, b"k"), Some(b"1".to_vec()));
    assert_eq!(version(&store), 1);
}
