//! Durable open: log replay, torn and corrupt records, and crashes during checkpoints.

use std::collections::BTreeMap;
use std::io::Write;

use super::*;

#[test]
fn durable_commits_survive_reopen_without_checkpoint() {
    let dir = TempDir::new();
    {
        let store = open(&dir);
        assert_eq!(put(&store, b"a", b"1"), 1);
        let mut tx = rw(&store);
        tx.set(b"b".to_vec(), b"2".to_vec()).unwrap();
        tx.set(b"c".to_vec(), b"3".to_vec()).unwrap();
        assert_eq!(tx.commit().unwrap().0, 2);
        assert_eq!(del(&store, b"a"), 3);
        assert_eq!(put(&store, b"b", b"22"), 4);
    }
    assert!(!dir.file(wal::CHECKPOINT_FILE).exists());
    let store = open(&dir);
    assert_eq!(get(&store, b"a"), None);
    assert_eq!(get(&store, b"b"), Some(b"22".to_vec()));
    assert_eq!(get(&store, b"c"), Some(b"3".to_vec()));
    assert_eq!(version(&store), 4);
    assert_eq!(
        store.key_count().unwrap(),
        2,
        "deleted keys are not resurrected"
    );
}

#[test]
fn versions_continue_monotonically_after_reopen() {
    let dir = TempDir::new();
    let mut seen = Vec::new();
    for round in 0..3u8 {
        let store = open(&dir);
        seen.push(put(&store, b"k", &[round]));
        seen.push(put(&store, b"k", &[round, round]));
        if round == 1 {
            store.checkpoint_durable().unwrap();
        }
    }
    assert_eq!(seen, vec![1, 2, 3, 4, 5, 6]);
    let store = open(&dir);
    assert_eq!(version(&store), 6);
    assert_eq!(get(&store, b"k"), Some(vec![2, 2]));
    assert_eq!(logged_versions(&dir.file(wal::WAL_FILE)), vec![5, 6]);
}

#[test]
fn checkpoint_then_more_commits_then_reopen() {
    let dir = TempDir::new();
    {
        let store = open(&dir);
        put(&store, b"a", b"1");
        put(&store, b"b", b"2");
        del(&store, b"b");
        store.checkpoint_durable().unwrap();
        assert!(dir.file(wal::CHECKPOINT_FILE).exists());
        assert!(!dir.file(wal::WAL_OLD_FILE).exists());
        put(&store, b"c", b"3");
        put(&store, b"a", b"11");
        put(&store, b"b", b"again");
    }
    let store = open(&dir);
    assert_eq!(get(&store, b"a"), Some(b"11".to_vec()));
    assert_eq!(get(&store, b"b"), Some(b"again".to_vec()));
    assert_eq!(get(&store, b"c"), Some(b"3".to_vec()));
    assert_eq!(version(&store), 6);
    assert_eq!(logged_versions(&dir.file(wal::WAL_FILE)), vec![4, 5, 6]);
    // The checkpoint alone records version 3.
    let checkpoint = MemStore::load(&dir.file(wal::CHECKPOINT_FILE)).unwrap();
    assert_eq!(version(&checkpoint), 3);
    assert_eq!(get(&checkpoint, b"b"), None);
}

#[test]
fn wal_size_grows_and_resets_after_checkpoint() {
    let dir = TempDir::new();
    let store = open(&dir);
    assert_eq!(store.wal_size(), 0);
    put(&store, b"a", b"1");
    let one = store.wal_size();
    assert!(one > 0);
    put(&store, b"b", b"2");
    assert!(store.wal_size() > one);
    assert_eq!(
        store.wal_size(),
        fs::metadata(dir.file(wal::WAL_FILE)).unwrap().len()
    );
    store.checkpoint_durable().unwrap();
    assert_eq!(store.wal_size(), 0);
    assert_eq!(fs::metadata(dir.file(wal::WAL_FILE)).unwrap().len(), 0);
    put(&store, b"c", b"3");
    assert!(store.wal_size() > 0);

    let plain = MemStore::new();
    put(&plain, b"a", b"1");
    assert_eq!(plain.wal_size(), 0);
    plain.checkpoint_durable().unwrap();
}

#[test]
fn torn_tail_recovers_all_earlier_commits() {
    let dir = TempDir::new();
    let wal_path = dir.file(wal::WAL_FILE);
    let before_last;
    {
        let store = open(&dir);
        put(&store, b"a", b"1");
        put(&store, b"b", b"2");
        before_last = store.wal_size();
        put(&store, b"c", b"3");
    }
    for cut in [1, 5, 9] {
        let len = fs::metadata(&wal_path).unwrap().len();
        if cut == 1 {
            wal::truncate_file(&wal_path, len - cut).unwrap();
        } else {
            // Re-append a torn copy of a frame: only `cut` bytes of it.
            let mut file = fs::OpenOptions::new().append(true).open(&wal_path).unwrap();
            let no_writes: BTreeMap<Vec<u8>, Option<Vec<u8>>> = BTreeMap::new();
            let frame = wal::encode_record(3, &no_writes).unwrap();
            file.write_all(&frame[..cut as usize]).unwrap();
        }
        let store = open(&dir);
        assert_eq!(get(&store, b"a"), Some(b"1".to_vec()));
        assert_eq!(get(&store, b"b"), Some(b"2".to_vec()));
        assert_eq!(get(&store, b"c"), None);
        assert_eq!(version(&store), 2);
        assert_eq!(store.wal_size(), before_last, "tail truncated");
    }
    {
        let store = open(&dir);
        assert_eq!(put(&store, b"d", b"4"), 3);
    }
    let store = open(&dir);
    assert_eq!(get(&store, b"d"), Some(b"4".to_vec()));
    assert_eq!(logged_versions(&wal_path), vec![1, 2, 3]);
}

#[test]
fn crc_corruption_stops_replay_at_the_bad_record() {
    let dir = TempDir::new();
    let wal_path = dir.file(wal::WAL_FILE);
    let (first_end, second_end);
    {
        let store = open(&dir);
        put(&store, b"a", b"1");
        first_end = store.wal_size();
        put(&store, b"b", b"2");
        second_end = store.wal_size();
        put(&store, b"c", b"3");
    }
    // Corrupt the last payload byte of the middle record; the record
    // after it is intact but must not be applied over the gap.
    let mut bytes = fs::read(&wal_path).unwrap();
    bytes[second_end as usize - 1] ^= 0x55;
    fs::write(&wal_path, &bytes).unwrap();

    let store = open(&dir);
    assert_eq!(get(&store, b"a"), Some(b"1".to_vec()));
    assert_eq!(get(&store, b"b"), None);
    assert_eq!(get(&store, b"c"), None);
    assert_eq!(version(&store), 1);
    assert_eq!(fs::metadata(&wal_path).unwrap().len(), first_end);
    assert_eq!(put(&store, b"z", b"26"), 2);
}

#[test]
fn version_gap_ends_replay() {
    let dir = TempDir::new();
    fs::create_dir_all(&dir.0).unwrap();
    let mut writes = BTreeMap::new();
    writes.insert(b"k".to_vec(), Some(b"v".to_vec()));
    let frames: Vec<Vec<u8>> = [1, 2, 4, 5]
        .into_iter()
        .map(|v| wal::encode_record(v, &writes).unwrap())
        .collect();
    fs::write(dir.file(wal::WAL_FILE), frames.concat()).unwrap();
    let store = open(&dir);
    assert_eq!(version(&store), 2);
    assert_eq!(logged_versions(&dir.file(wal::WAL_FILE)), vec![1, 2]);
}

#[test]
fn crash_after_rotation_before_new_log_exists() {
    // Rotation renamed the log, then the process died before creating a
    // fresh one: only `.old` and no checkpoint.
    let dir = TempDir::new();
    {
        let store = open(&dir);
        put(&store, b"a", b"1");
        put(&store, b"b", b"2");
    }
    fs::rename(dir.file(wal::WAL_FILE), dir.file(wal::WAL_OLD_FILE)).unwrap();
    let store = open(&dir);
    assert_eq!(get(&store, b"a"), Some(b"1".to_vec()));
    assert_eq!(get(&store, b"b"), Some(b"2".to_vec()));
    assert_eq!(version(&store), 2);
    assert!(dir.file(wal::WAL_FILE).exists());
}

#[test]
fn crash_between_rotation_and_checkpoint() {
    let dir = TempDir::new();
    {
        let store = open(&dir);
        put(&store, b"a", b"1");
        store.checkpoint_durable().unwrap(); // records.kv at version 1
        put(&store, b"b", b"2");
        del(&store, b"a");
    }
    // Rotation happened (log moved to `.old`) but the checkpoint at
    // version 3 was never written; commits continued in a new log.
    fs::rename(dir.file(wal::WAL_FILE), dir.file(wal::WAL_OLD_FILE)).unwrap();
    {
        let store = open(&dir);
        assert_eq!(version(&store), 3);
        put(&store, b"c", b"3");
    }
    let store = open(&dir);
    assert_eq!(get(&store, b"a"), None);
    assert_eq!(get(&store, b"b"), Some(b"2".to_vec()));
    assert_eq!(get(&store, b"c"), Some(b"3".to_vec()));
    assert_eq!(version(&store), 4);

    // The next checkpoint must merge into the existing `.old` rather
    // than overwrite it, then clean up.
    store.checkpoint_durable().unwrap();
    assert!(!dir.file(wal::WAL_OLD_FILE).exists());
    put(&store, b"d", b"4");
    drop(store);
    let store = open(&dir);
    assert_eq!(get(&store, b"a"), None);
    assert_eq!(get(&store, b"c"), Some(b"3".to_vec()));
    assert_eq!(get(&store, b"d"), Some(b"4".to_vec()));
    assert_eq!(version(&store), 5);
}

#[test]
fn crash_after_checkpoint_before_old_log_deleted() {
    let dir = TempDir::new();
    {
        let store = open(&dir);
        put(&store, b"k", b"1");
        del(&store, b"k");
        put(&store, b"x", b"1");
        let rotated = fs::read(dir.file(wal::WAL_FILE)).unwrap();
        store.checkpoint_durable().unwrap();
        put(&store, b"k", b"3");
        // Put back the log the checkpoint already covers, as if the
        // process died before deleting it.
        fs::write(dir.file(wal::WAL_OLD_FILE), rotated).unwrap();
    }
    let store = open(&dir);
    assert_eq!(get(&store, b"k"), Some(b"3".to_vec()));
    assert_eq!(get(&store, b"x"), Some(b"1".to_vec()));
    assert_eq!(version(&store), 4);
    assert_eq!(put(&store, b"y", b"5"), 5);
    store.checkpoint_durable().unwrap();
    drop(store);
    let store = open(&dir);
    assert_eq!(version(&store), 5);
    assert_eq!(get(&store, b"k"), Some(b"3".to_vec()));
}

#[test]
fn crash_while_merging_into_old_log() {
    let dir = TempDir::new();
    {
        let store = open(&dir);
        put(&store, b"a", b"1");
        put(&store, b"b", b"2");
    }
    fs::rename(dir.file(wal::WAL_FILE), dir.file(wal::WAL_OLD_FILE)).unwrap();
    {
        let store = open(&dir);
        put(&store, b"c", b"3");
        put(&store, b"d", b"4");
    }
    // A merge copied the active log into `.old` but died part-way: `.old`
    // ends with one full duplicate record and a torn one, and the
    // active log was never emptied.
    let active = fs::read(dir.file(wal::WAL_FILE)).unwrap();
    let first_len = wal::read_log(&dir.file(wal::WAL_FILE)).unwrap().records[1].0 as usize;
    let mut old = fs::OpenOptions::new()
        .append(true)
        .open(dir.file(wal::WAL_OLD_FILE))
        .unwrap();
    old.write_all(&active[..first_len + 5]).unwrap();
    drop(old);

    let store = open(&dir);
    for (k, v) in [(b"a", b"1"), (b"b", b"2"), (b"c", b"3"), (b"d", b"4")] {
        assert_eq!(get(&store, k), Some(v.to_vec()));
    }
    assert_eq!(version(&store), 4);
    assert_eq!(logged_versions(&dir.file(wal::WAL_OLD_FILE)), vec![1, 2, 3]);
    assert_eq!(logged_versions(&dir.file(wal::WAL_FILE)), vec![3, 4]);
    assert_eq!(put(&store, b"e", b"5"), 5);
    store.checkpoint_durable().unwrap();
    drop(store);
    let store = open(&dir);
    assert_eq!(version(&store), 5);
    assert_eq!(get(&store, b"e"), Some(b"5".to_vec()));
}

#[test]
fn crash_during_checkpoint_write_leaves_only_ignored_temp_file() {
    let dir = TempDir::new();
    {
        let store = open(&dir);
        put(&store, b"a", b"1");
        store.checkpoint_durable().unwrap();
        put(&store, b"b", b"2");
    }
    let stale = dir.file("records.tmp.999999.0");
    fs::write(&stale, b"half-written garbage").unwrap();
    fs::rename(dir.file(wal::WAL_FILE), dir.file(wal::WAL_OLD_FILE)).unwrap();
    let store = open(&dir);
    assert!(!stale.exists(), "stale temporary checkpoint removed");
    assert_eq!(get(&store, b"a"), Some(b"1".to_vec()));
    assert_eq!(get(&store, b"b"), Some(b"2".to_vec()));
    assert_eq!(version(&store), 2);
}

#[test]
fn sync_never_still_replays_on_clean_reopen() {
    let dir = TempDir::new();
    {
        let store = MemStore::open_durable(&dir.0, SyncMode::Never).unwrap();
        put(&store, b"a", b"1");
        put(&store, b"b", b"2");
    }
    let store = MemStore::open_durable(&dir.0, SyncMode::Never).unwrap();
    assert_eq!(get(&store, b"a"), Some(b"1".to_vec()));
    assert_eq!(get(&store, b"b"), Some(b"2".to_vec()));
    assert_eq!(version(&store), 2);
}
