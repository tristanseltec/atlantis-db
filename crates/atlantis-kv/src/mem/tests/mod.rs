//! Tests for the in-memory MVCC backend, and helpers shared by them.

mod commit;
mod group_commit;
mod mvcc;
mod recovery;

use std::fs;
use std::path::Path;

use atlantis_core::AtlantisError;

use crate::keys::KeyRange;
use crate::kv::{Isolation, KvStore, KvTransaction, TxMode};
use crate::wal;
use crate::{MemStore, SyncMode};

pub(super) fn rw(store: &MemStore) -> Box<dyn KvTransaction> {
    store.begin(TxMode::ReadWrite, Isolation::Snapshot).unwrap()
}

pub(super) fn range(start: &[u8], end: &[u8]) -> KeyRange {
    KeyRange {
        start: start.to_vec(),
        end: end.to_vec(),
    }
}

/// A unique temporary directory removed on drop (also when a test fails).
pub(super) struct TempDir(pub(super) std::path::PathBuf);

impl TempDir {
    pub(super) fn new() -> Self {
        TempDir(std::env::temp_dir().join(format!(
            "atlantis-kv-durable-{}",
            atlantis_core::Ulid::new()
        )))
    }

    pub(super) fn file(&self, name: &str) -> std::path::PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn open(dir: &TempDir) -> MemStore {
    MemStore::open_durable(&dir.0, SyncMode::Every).unwrap()
}

pub(super) fn put(store: &MemStore, key: &[u8], value: &[u8]) -> u64 {
    let mut tx = rw(store);
    tx.set(key.to_vec(), value.to_vec()).unwrap();
    tx.commit().unwrap().0
}

pub(super) fn del(store: &MemStore, key: &[u8]) -> u64 {
    let mut tx = rw(store);
    tx.delete(key).unwrap();
    tx.commit().unwrap().0
}

pub(super) fn get(store: &MemStore, key: &[u8]) -> Option<Vec<u8>> {
    rw(store).get(key).unwrap()
}

pub(super) fn version(store: &MemStore) -> u64 {
    store.current_version().unwrap().0
}

pub(super) fn logged_versions(path: &Path) -> Vec<u64> {
    wal::read_log(path)
        .unwrap()
        .records
        .into_iter()
        .map(|(_, r)| r.version)
        .collect()
}
