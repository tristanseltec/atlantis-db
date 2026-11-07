//! In-memory MVCC backend (`mem://`).
//!
//! Each key keeps a list of committed versions. Transactions read the newest
//! version at or below their snapshot, buffer writes locally, and validate
//! at commit time. Old versions are pruned once no open snapshot can read
//! them. The store can be checkpointed to a single file and loaded back.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::ops::Bound;
use std::path::Path;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use atlantis_core::AtlantisError;

use crate::keys::KeyRange;
use crate::kv::{Isolation, KvPair, KvStore, KvTransaction, TxMode, Versionstamp};

/// One committed version: the commit version and the value, or `None` for
/// a deletion.
type Version = (u64, Option<Vec<u8>>);

const CHECKPOINT_MAGIC: &[u8; 4] = b"ATKV";
const CHECKPOINT_FORMAT: u8 = 1;

#[derive(Default)]
struct Inner {
    data: BTreeMap<Vec<u8>, Vec<Version>>,
    /// Version of the latest commit.
    version: u64,
    /// Open snapshots: snapshot version → number of transactions using it.
    active: BTreeMap<u64, usize>,
}

impl Inner {
    fn latest_version(&self, key: &[u8]) -> Option<u64> {
        self.data
            .get(key)
            .and_then(|versions| versions.last())
            .map(|(v, _)| *v)
    }

    fn read_at(&self, key: &[u8], snapshot: u64) -> Option<&Vec<u8>> {
        let versions = self.data.get(key)?;
        versions
            .iter()
            .rev()
            .find(|(v, _)| *v <= snapshot)?
            .1
            .as_ref()
    }

    /// Oldest version any open transaction may still read.
    fn horizon(&self) -> u64 {
        self.active.keys().next().copied().unwrap_or(self.version)
    }

    /// Drops versions of `key` that no snapshot at or after `horizon` can see.
    fn prune(&mut self, key: &[u8], horizon: u64) {
        let Some(versions) = self.data.get_mut(key) else {
            return;
        };
        // Keep the newest version at or below the horizon and everything newer.
        if let Some(keep_from) = versions.iter().rposition(|(v, _)| *v <= horizon) {
            versions.drain(..keep_from);
        }
        if versions.len() == 1 && versions[0].1.is_none() && versions[0].0 <= horizon {
            self.data.remove(key);
        }
    }

    fn release(&mut self, snapshot: u64) {
        if let Some(count) = self.active.get_mut(&snapshot) {
            *count -= 1;
            if *count == 0 {
                self.active.remove(&snapshot);
            }
        }
    }
}

/// The in-memory MVCC key-value store. Cheap to clone; clones share data.
#[derive(Clone, Default)]
pub struct MemStore {
    inner: Arc<RwLock<Inner>>,
}

fn poisoned() -> AtlantisError {
    AtlantisError::Storage("key-value store lock poisoned".into())
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> Result<RwLockReadGuard<'_, Inner>, AtlantisError> {
        self.inner.read().map_err(|_| poisoned())
    }

    fn write(&self) -> Result<RwLockWriteGuard<'_, Inner>, AtlantisError> {
        self.inner.write().map_err(|_| poisoned())
    }

    /// Prunes every key down to what open snapshots can still read.
    pub fn gc(&self) -> Result<(), AtlantisError> {
        let mut inner = self.write()?;
        let horizon = inner.horizon();
        let keys: Vec<Vec<u8>> = inner.data.keys().cloned().collect();
        for key in keys {
            inner.prune(&key, horizon);
        }
        Ok(())
    }

    /// Number of keys holding at least one version (live or deleted).
    pub fn key_count(&self) -> Result<usize, AtlantisError> {
        Ok(self.read()?.data.len())
    }

    /// Total number of stored versions across all keys.
    pub fn version_count(&self) -> Result<usize, AtlantisError> {
        Ok(self.read()?.data.values().map(Vec::len).sum())
    }

    /// The latest commit's versionstamp.
    pub fn current_version(&self) -> Result<Versionstamp, AtlantisError> {
        Ok(Versionstamp(self.read()?.version))
    }

    /// Writes the latest committed state to `path` atomically (write to a
    /// temporary file, sync, then rename). Uncommitted writes are excluded.
    pub fn checkpoint(&self, path: &Path) -> Result<(), AtlantisError> {
        let (version, pairs) = {
            let inner = self.read()?;
            let pairs: Vec<KvPair> = inner
                .data
                .iter()
                .filter_map(|(k, versions)| {
                    versions
                        .last()
                        .and_then(|(_, v)| v.as_ref())
                        .map(|v| (k.clone(), v.clone()))
                })
                .collect();
            (inner.version, pairs)
        };
        let body = bincode::serialize(&(version, pairs))
            .map_err(|e| AtlantisError::Serialization(e.to_string()))?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        let mut file = fs::File::create(&tmp)?;
        file.write_all(CHECKPOINT_MAGIC)?;
        file.write_all(&[CHECKPOINT_FORMAT])?;
        file.write_all(&body)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Loads a store from a checkpoint written by [`checkpoint`](Self::checkpoint).
    pub fn load(path: &Path) -> Result<Self, AtlantisError> {
        let bytes = fs::read(path)?;
        let header = CHECKPOINT_MAGIC.len() + 1;
        if bytes.len() < header || &bytes[..4] != CHECKPOINT_MAGIC {
            return Err(AtlantisError::Serialization(format!(
                "{} is not a key-value checkpoint",
                path.display()
            )));
        }
        if bytes[4] != CHECKPOINT_FORMAT {
            return Err(AtlantisError::Serialization(format!(
                "unsupported checkpoint format {}",
                bytes[4]
            )));
        }
        let (version, pairs): (u64, Vec<KvPair>) = bincode::deserialize(&bytes[header..])
            .map_err(|e| AtlantisError::Serialization(e.to_string()))?;
        let data = pairs
            .into_iter()
            .map(|(k, v)| (k, vec![(version, Some(v))]))
            .collect();
        Ok(MemStore {
            inner: Arc::new(RwLock::new(Inner {
                data,
                version,
                active: BTreeMap::new(),
            })),
        })
    }

    /// Opens the checkpoint at `path` if it exists, otherwise an empty store.
    pub fn open_or_create(path: &Path) -> Result<Self, AtlantisError> {
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::new())
        }
    }
}

impl KvStore for MemStore {
    fn begin(
        &self,
        mode: TxMode,
        isolation: Isolation,
    ) -> Result<Box<dyn KvTransaction>, AtlantisError> {
        let snapshot = {
            let mut inner = self.write()?;
            let snapshot = inner.version;
            *inner.active.entry(snapshot).or_insert(0) += 1;
            snapshot
        };
        Ok(Box::new(MemTransaction {
            store: self.clone(),
            snapshot,
            mode,
            isolation,
            writes: BTreeMap::new(),
            reads: HashSet::new(),
            scans: Vec::new(),
            finished: false,
        }))
    }
}

struct MemTransaction {
    store: MemStore,
    snapshot: u64,
    mode: TxMode,
    isolation: Isolation,
    writes: BTreeMap<Vec<u8>, Option<Vec<u8>>>,
    /// Keys read (tracked only for serializable transactions).
    reads: HashSet<Vec<u8>>,
    /// Ranges scanned (tracked only for serializable transactions).
    scans: Vec<KeyRange>,
    finished: bool,
}

impl MemTransaction {
    fn check_open(&self) -> Result<(), AtlantisError> {
        if self.finished {
            Err(AtlantisError::TxFinished)
        } else {
            Ok(())
        }
    }

    fn check_writable(&self) -> Result<(), AtlantisError> {
        self.check_open()?;
        if self.mode == TxMode::ReadOnly {
            return Err(AtlantisError::TxReadOnly);
        }
        Ok(())
    }

    fn tracks_reads(&self) -> bool {
        self.isolation == Isolation::Serializable && self.mode == TxMode::ReadWrite
    }

    fn finish(&mut self) {
        if !self.finished {
            self.finished = true;
            if let Ok(mut inner) = self.store.inner.write() {
                inner.release(self.snapshot);
            }
            self.writes.clear();
            self.reads.clear();
            self.scans.clear();
        }
    }

    /// Returns a conflict description if a concurrent commit invalidates
    /// this transaction.
    fn conflict(&self, inner: &Inner) -> Option<String> {
        let changed = |key: &[u8]| inner.latest_version(key).is_some_and(|v| v > self.snapshot);
        if let Some(key) = self.writes.keys().find(|k| changed(k)) {
            return Some(format!("write-write conflict on key {}", escape(key)));
        }
        if self.isolation == Isolation::Serializable {
            if let Some(key) = self.reads.iter().find(|k| changed(k)) {
                return Some(format!(
                    "a key read by this transaction changed: {}",
                    escape(key)
                ));
            }
            for range in &self.scans {
                let bounds = (
                    Bound::Included(range.start.clone()),
                    Bound::Excluded(range.end.clone()),
                );
                if inner
                    .data
                    .range::<Vec<u8>, _>(bounds)
                    .any(|(_, versions)| versions.last().is_some_and(|(v, _)| *v > self.snapshot))
                {
                    return Some("a range scanned by this transaction changed".into());
                }
            }
        }
        None
    }
}

fn escape(key: &[u8]) -> String {
    key.iter()
        .flat_map(|b| std::ascii::escape_default(*b))
        .map(char::from)
        .collect()
}

impl KvTransaction for MemTransaction {
    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, AtlantisError> {
        self.check_open()?;
        if let Some(local) = self.writes.get(key) {
            return Ok(local.clone());
        }
        if self.tracks_reads() {
            self.reads.insert(key.to_vec());
        }
        let inner = self.store.read()?;
        Ok(inner.read_at(key, self.snapshot).cloned())
    }

    fn set(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), AtlantisError> {
        self.check_writable()?;
        self.writes.insert(key, Some(value));
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), AtlantisError> {
        self.check_writable()?;
        self.writes.insert(key.to_vec(), None);
        Ok(())
    }

    fn scan(
        &mut self,
        range: &KeyRange,
        limit: Option<usize>,
    ) -> Result<Vec<KvPair>, AtlantisError> {
        self.check_open()?;
        if range.start >= range.end {
            return Ok(Vec::new());
        }
        if self.tracks_reads() {
            self.scans.push(range.clone());
        }
        let mut merged: BTreeMap<Vec<u8>, Vec<u8>> = {
            let inner = self.store.read()?;
            let bounds = (
                Bound::Included(range.start.clone()),
                Bound::Excluded(range.end.clone()),
            );
            inner
                .data
                .range::<Vec<u8>, _>(bounds)
                .filter_map(|(k, _)| {
                    inner
                        .read_at(k, self.snapshot)
                        .map(|v| (k.clone(), v.clone()))
                })
                .collect()
        };
        let local = self.writes.range::<Vec<u8>, _>((
            Bound::Included(range.start.clone()),
            Bound::Excluded(range.end.clone()),
        ));
        for (k, v) in local {
            match v {
                Some(v) => merged.insert(k.clone(), v.clone()),
                None => merged.remove(k),
            };
        }
        let limit = limit.unwrap_or(usize::MAX);
        Ok(merged.into_iter().take(limit).collect())
    }

    fn commit(&mut self) -> Result<Versionstamp, AtlantisError> {
        self.check_open()?;
        if self.writes.is_empty() {
            let snapshot = self.snapshot;
            self.finish();
            return Ok(Versionstamp(snapshot));
        }
        let store = self.store.clone();
        let mut inner = store.write()?;
        if let Some(reason) = self.conflict(&inner) {
            drop(inner);
            self.finish();
            return Err(AtlantisError::TxConflict(reason));
        }
        inner.version += 1;
        let version = inner.version;
        let writes = std::mem::take(&mut self.writes);
        // Release our snapshot before pruning so it does not hold back the horizon.
        inner.release(self.snapshot);
        self.finished = true;
        let horizon = inner.horizon();
        for (key, value) in writes {
            inner
                .data
                .entry(key.clone())
                .or_default()
                .push((version, value));
            inner.prune(&key, horizon);
        }
        Ok(Versionstamp(version))
    }

    fn cancel(&mut self) -> Result<(), AtlantisError> {
        self.check_open()?;
        self.finish();
        Ok(())
    }

    fn is_finished(&self) -> bool {
        self.finished
    }

    fn snapshot(&self) -> Versionstamp {
        Versionstamp(self.snapshot)
    }
}

impl Drop for MemTransaction {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rw(store: &MemStore) -> Box<dyn KvTransaction> {
        store.begin(TxMode::ReadWrite, Isolation::Snapshot).unwrap()
    }

    fn range(start: &[u8], end: &[u8]) -> KeyRange {
        KeyRange {
            start: start.to_vec(),
            end: end.to_vec(),
        }
    }

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
        let dir =
            std::env::temp_dir().join(format!("atlantis-kv-test-{}", atlantis_core::Ulid::new()));
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
}
