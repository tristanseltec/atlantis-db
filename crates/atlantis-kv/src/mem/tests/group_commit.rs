//! Concurrent commits sharing syncs, interval syncing and concurrent checkpoints.

use super::*;

#[test]
fn concurrent_commits_share_syncs_and_survive_reopen() {
    let dir = TempDir::new();
    let store = open(&dir);
    let threads = 8;
    let per_thread = 100;
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            let store = store.clone();
            std::thread::spawn(move || {
                let mut versions = Vec::new();
                for i in 0..per_thread {
                    let key = format!("t{t}-{i}").into_bytes();
                    versions.push(put(&store, &key, b"v"));
                }
                versions
            })
        })
        .collect();
    let mut all: Vec<u64> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();
    all.sort_unstable();
    let total = (threads * per_thread) as u64;
    assert_eq!(
        all,
        (1..=total).collect::<Vec<_>>(),
        "versions are unique and dense"
    );
    assert_eq!(version(&store), total);
    assert!(store.sync_count() <= total);
    drop(store);
    let store = open(&dir);
    assert_eq!(version(&store), total);
    for t in 0..threads {
        for i in 0..per_thread {
            assert!(get(&store, format!("t{t}-{i}").as_bytes()).is_some());
        }
    }
}

#[test]
fn interval_mode_publishes_at_once_and_syncs_in_background() {
    let dir = TempDir::new();
    let store = MemStore::open_durable(
        &dir.0,
        SyncMode::Interval(std::time::Duration::from_millis(5)),
    )
    .unwrap();
    for i in 0..20u8 {
        put(&store, &[i], b"v");
    }
    assert_eq!(version(&store), 20);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while store.durability.synced() < 20 {
        assert!(
            std::time::Instant::now() < deadline,
            "background sync never ran"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    drop(store);
    let store = MemStore::open_durable(&dir.0, SyncMode::Never).unwrap();
    assert_eq!(version(&store), 20);
}

#[test]
fn commits_during_concurrent_checkpoints_are_not_lost() {
    let dir = TempDir::new();
    let store = open(&dir);
    let writer = {
        let store = store.clone();
        std::thread::spawn(move || {
            for i in 0..200u32 {
                put(&store, &i.to_be_bytes(), &i.to_le_bytes());
            }
        })
    };
    for _ in 0..20 {
        store.checkpoint_durable().unwrap();
    }
    writer.join().unwrap();
    drop(store);
    let store = open(&dir);
    assert_eq!(version(&store), 200);
    for i in 0..200u32 {
        assert_eq!(
            get(&store, &i.to_be_bytes()),
            Some(i.to_le_bytes().to_vec())
        );
    }
}
