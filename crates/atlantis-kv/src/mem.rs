//! In-memory MVCC backend (`mem://`).
//!
//! Each key keeps a list of committed versions. Transactions read the newest
//! version at or below their snapshot, buffer writes locally, and validate
//! at commit time. Old versions are pruned once no open snapshot can read
//! them. The store can be checkpointed to a single file and loaded back.
//!
//! # Durability
//!
//! [`MemStore::new`], [`MemStore::load`] and [`MemStore::open_or_create`]
//! give a purely in-memory store: commits are lost unless
//! [`checkpoint`](MemStore::checkpoint) is called, and no log is written.
//! [`MemStore::open_durable`] instead backs the store with a directory
//! holding a checkpoint (`records.kv`) and a write-ahead log (`records.wal`,
//! format in [`crate::wal`]):
//!
//! - **Commit.** After conflict validation, and while still holding the
//!   store-wide write lock, a commit appends one log record with its new
//!   version and writes (synced first with [`SyncMode::Every`]). Only then
//!   are the writes applied in memory and the commit acknowledged. If the
//!   append fails the commit fails, nothing is applied and its snapshot is
//!   released. Commits without writes log nothing; cancelled or conflicting
//!   transactions never reach the log.
//! - **Checkpoint.** [`checkpoint_durable`](MemStore::checkpoint_durable)
//!   (1) under the write lock rotates `records.wal` to `records.wal.old`,
//!   starts an empty `records.wal` and copies the latest state at version
//!   V; (2) without the lock writes `records.kv` atomically, recording V;
//!   (3) deletes `records.wal.old` and syncs the directory. Commits made
//!   during (2) and (3) go to the new log. If `.old` still exists from an
//!   earlier failed checkpoint, the active log is appended to it (and synced)
//!   instead of replacing it, so no uncovered record is ever overwritten.
//! - **Recovery.** Opening loads `records.kv` (version C, or 0 if absent),
//!   then replays `records.wal.old` and `records.wal` in that order. Records
//!   at or below the current version are skipped (covered by the checkpoint,
//!   or duplicated by an interrupted merge); any other record must carry
//!   exactly the next version. Replay of a file stops at its first bad frame
//!   (torn or corrupt — including corruption in the middle followed by
//!   valid frames, which are discarded) or at a version gap, and the file is
//!   truncated there. The recovered state is thus always a prefix of the
//!   commit history, and a crash at any point of the commit or checkpoint
//!   sequence recovers every commit whose log record reached disk.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::ops::Bound;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use atlantis_core::AtlantisError;

use crate::keys::KeyRange;
use crate::kv::{Isolation, KvPair, KvStore, KvTransaction, TxMode, Versionstamp};
use crate::wal::{self, SyncMode, Wal, WalRecord, CHECKPOINT_FILE, WAL_FILE, WAL_OLD_FILE};

/// One committed version: the commit version and the value, or `None` for
/// a deletion.
type Version = (u64, Option<Vec<u8>>);

const CHECKPOINT_MAGIC: &[u8; 4] = b"ATKV";
const CHECKPOINT_FORMAT: u8 = 1;
/// Extension prefix of temporary checkpoint files (`records.tmp.<pid>.<n>`).
const CHECKPOINT_TMP_EXTENSION: &str = "tmp";

#[derive(Default)]
struct Inner {
    data: BTreeMap<Vec<u8>, Vec<Version>>,
    /// Version of the latest commit.
    version: u64,
    /// Open snapshots: snapshot version → number of transactions using it.
    active: BTreeMap<u64, usize>,
    /// Write-ahead log; `Some` only for stores opened with
    /// [`MemStore::open_durable`].
    wal: Option<Wal>,
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

    /// The latest committed value of every live key.
    fn latest_pairs(&self) -> Vec<KvPair> {
        self.data
            .iter()
            .filter_map(|(k, versions)| {
                versions
                    .last()
                    .and_then(|(_, v)| v.as_ref())
                    .map(|v| (k.clone(), v.clone()))
            })
            .collect()
    }

    /// Applies a recovered log record. Recovery has no open snapshots, so
    /// each key keeps only its newest version and deleted keys disappear.
    fn apply_replayed(&mut self, record: WalRecord) {
        for (key, value) in record.writes {
            match value {
                Some(value) => {
                    self.data.insert(key, vec![(record.version, Some(value))]);
                }
                None => {
                    self.data.remove(&key);
                }
            }
        }
        self.version = record.version;
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
    /// Serializes checkpoints so concurrent calls never interleave writes.
    checkpoint_lock: Arc<Mutex<()>>,
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

    fn lock_checkpoints(&self) -> MutexGuard<'_, ()> {
        self.checkpoint_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Writes the latest committed state to `path` atomically (write to a
    /// temporary file, sync, then rename). Uncommitted writes are excluded.
    pub fn checkpoint(&self, path: &Path) -> Result<(), AtlantisError> {
        let _serialized = self.lock_checkpoints();
        let (version, pairs) = {
            let inner = self.read()?;
            (inner.version, inner.latest_pairs())
        };
        write_checkpoint(path, version, &pairs)
    }

    /// Loads a store from a checkpoint written by [`checkpoint`](Self::checkpoint).
    pub fn load(path: &Path) -> Result<Self, AtlantisError> {
        Ok(Self::from_inner(read_checkpoint(path)?))
    }

    /// Opens the checkpoint at `path` if it exists, otherwise an empty store.
    pub fn open_or_create(path: &Path) -> Result<Self, AtlantisError> {
        if path.exists() {
            Self::load(path)
        } else {
            Ok(Self::new())
        }
    }

    fn from_inner(inner: Inner) -> Self {
        MemStore {
            inner: Arc::new(RwLock::new(inner)),
            checkpoint_lock: Arc::default(),
        }
    }

    /// Opens a durable store in `dir`, creating the directory if needed.
    ///
    /// Loads `dir/records.kv` if present, then replays `dir/records.wal.old`
    /// (if present) and `dir/records.wal`, skipping records whose version is
    /// at or below the state already loaded. A torn or corrupt record (bad
    /// length, CRC or encoding) ends replay of its file and the file is
    /// truncated to the last good record. Corruption in the middle of a file
    /// is handled the same way: replay stops at the first bad record and the
    /// valid records after it are discarded, so the recovered state is always
    /// a prefix of the commit history. A version gap (a record newer than the
    /// next expected version) also ends replay of its file. Every later
    /// commit is logged before it is acknowledged; see the
    /// [module docs](self) for the full protocol.
    pub fn open_durable(dir: &Path, sync: SyncMode) -> Result<MemStore, AtlantisError> {
        fs::create_dir_all(dir)?;
        remove_stale_checkpoint_temps(dir)?;
        let checkpoint = dir.join(CHECKPOINT_FILE);
        let mut inner = if checkpoint.exists() {
            read_checkpoint(&checkpoint)?
        } else {
            Inner::default()
        };
        for name in [WAL_OLD_FILE, WAL_FILE] {
            let path = dir.join(name);
            if !path.exists() {
                continue;
            }
            let scan = wal::read_log(&path)?;
            let mut keep = scan.valid_len;
            for (offset, record) in scan.records {
                if record.version <= inner.version {
                    continue;
                }
                if record.version != inner.version + 1 {
                    keep = offset;
                    break;
                }
                inner.apply_replayed(record);
            }
            if keep < scan.file_len {
                wal::truncate_file(&path, keep)?;
            }
        }
        inner.wal = Some(Wal::open(dir, sync)?);
        Ok(Self::from_inner(inner))
    }

    /// Writes a checkpoint of the latest committed state to `records.kv` and
    /// drops the log records it covers. Commits are blocked only while the
    /// log is rotated and the state is copied, not while the file is written.
    /// Does nothing for a store not opened with
    /// [`open_durable`](Self::open_durable).
    pub fn checkpoint_durable(&self) -> Result<(), AtlantisError> {
        let _serialized = self.lock_checkpoints();
        let (dir, version, pairs) = {
            let mut inner = self.write()?;
            let Some(wal) = inner.wal.as_mut() else {
                return Ok(());
            };
            wal.rotate()?;
            let dir = wal.dir().to_path_buf();
            (dir, inner.version, inner.latest_pairs())
        };
        write_checkpoint(&dir.join(CHECKPOINT_FILE), version, &pairs)?;
        match fs::remove_file(dir.join(WAL_OLD_FILE)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        wal::sync_dir(&dir)?;
        Ok(())
    }

    /// Bytes currently in the active write-ahead log (0 for non-durable
    /// stores).
    pub fn wal_size(&self) -> u64 {
        self.inner
            .read()
            .map(|inner| inner.wal.as_ref().map_or(0, Wal::size))
            .unwrap_or(0)
    }
}

/// Writes a checkpoint of `pairs` at `version` to `path` atomically: write a
/// uniquely named temporary file, sync it, rename it over `path`, then sync
/// the directory.
fn write_checkpoint(path: &Path, version: u64, pairs: &[KvPair]) -> Result<(), AtlantisError> {
    let body = bincode::serialize(&(version, pairs))
        .map_err(|e| AtlantisError::Serialization(e.to_string()))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "{CHECKPOINT_TMP_EXTENSION}.{}.{}",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(CHECKPOINT_MAGIC)?;
        file.write_all(&[CHECKPOINT_FORMAT])?;
        file.write_all(&body)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    // Make the rename itself durable.
    if let Some(dir) = path.parent() {
        wal::sync_dir(dir)?;
    }
    Ok(())
}

/// Removes temporary files left in `dir` by a checkpoint of `records.kv`
/// that crashed before its rename. They are never read.
fn remove_stale_checkpoint_temps(dir: &Path) -> Result<(), AtlantisError> {
    let prefix = Path::new(CHECKPOINT_FILE).with_extension(format!("{CHECKPOINT_TMP_EXTENSION}."));
    let prefix = prefix.to_string_lossy();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let stale = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(prefix.as_ref()));
        if stale && entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Reads a checkpoint written by [`write_checkpoint`].
fn read_checkpoint(path: &Path) -> Result<Inner, AtlantisError> {
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
    Ok(Inner {
        data,
        version,
        ..Inner::default()
    })
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
        let version = inner.version + 1;
        if let Some(wal) = inner.wal.as_mut() {
            let logged =
                wal::encode_record(version, &self.writes).and_then(|frame| wal.append(&frame));
            if let Err(e) = logged {
                drop(inner);
                self.finish();
                return Err(e);
            }
        }
        inner.version = version;
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

    /// A unique temporary directory removed on drop (also when a test fails).
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new() -> Self {
            TempDir(std::env::temp_dir().join(format!(
                "atlantis-kv-durable-{}",
                atlantis_core::Ulid::new()
            )))
        }

        fn file(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn open(dir: &TempDir) -> MemStore {
        MemStore::open_durable(&dir.0, SyncMode::Every).unwrap()
    }

    fn put(store: &MemStore, key: &[u8], value: &[u8]) -> u64 {
        let mut tx = rw(store);
        tx.set(key.to_vec(), value.to_vec()).unwrap();
        tx.commit().unwrap().0
    }

    fn del(store: &MemStore, key: &[u8]) -> u64 {
        let mut tx = rw(store);
        tx.delete(key).unwrap();
        tx.commit().unwrap().0
    }

    fn get(store: &MemStore, key: &[u8]) -> Option<Vec<u8>> {
        rw(store).get(key).unwrap()
    }

    fn version(store: &MemStore) -> u64 {
        store.current_version().unwrap().0
    }

    fn logged_versions(path: &Path) -> Vec<u64> {
        wal::read_log(path)
            .unwrap()
            .records
            .into_iter()
            .map(|(_, r)| r.version)
            .collect()
    }

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
}
