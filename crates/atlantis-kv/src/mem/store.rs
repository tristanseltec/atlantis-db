//! [`MemStore`]: the public handle, its constructors, durable open and
//! recovery, checkpoints and statistics.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use atlantis_core::AtlantisError;

use crate::kv::Versionstamp;
use crate::wal::{self, SyncMode, Wal, CHECKPOINT_FILE, WAL_FILE, WAL_OLD_FILE};

use super::checkpoint::{read_checkpoint, remove_stale_checkpoint_temps, write_checkpoint};
use super::durability::{spawn_interval_syncer, Durability};
use super::inner::Inner;

/// The in-memory MVCC key-value store. Cheap to clone; clones share data.
#[derive(Clone, Default)]
pub struct MemStore {
    pub(super) inner: Arc<RwLock<Inner>>,
    /// Serializes checkpoints so concurrent calls never interleave writes.
    checkpoint_lock: Arc<Mutex<()>>,
    pub(super) durability: Arc<Durability>,
}

fn poisoned() -> AtlantisError {
    AtlantisError::Storage("key-value store lock poisoned".into())
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub(super) fn read(&self) -> Result<RwLockReadGuard<'_, Inner>, AtlantisError> {
        self.inner.read().map_err(|_| poisoned())
    }

    #[inline]
    pub(super) fn write(&self) -> Result<RwLockWriteGuard<'_, Inner>, AtlantisError> {
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

    fn from_inner(mut inner: Inner) -> Self {
        inner.staged = inner.version;
        let durability = Durability::default();
        {
            let mut st = durability.lock();
            st.appended = inner.version;
            st.synced = inner.version;
        }
        *durability.handle.lock().unwrap_or_else(|e| e.into_inner()) =
            inner.wal.as_ref().map(Wal::sync_handle);
        let interval = match inner.wal.as_ref().map(Wal::sync_mode) {
            Some(SyncMode::Interval(d)) => Some(d),
            _ => None,
        };
        let store = MemStore {
            inner: Arc::new(RwLock::new(inner)),
            checkpoint_lock: Arc::default(),
            durability: Arc::new(durability),
        };
        if let Some(every) = interval {
            spawn_interval_syncer(Arc::downgrade(&store.durability), every);
        }
        store
    }

    /// Publishes staged commits that are durable. Returns the store lock.
    pub(super) fn publish_durable(&self) -> Result<RwLockWriteGuard<'_, Inner>, AtlantisError> {
        let mut inner = self.write()?;
        let durable = self.durability.synced();
        inner.publish_durable(durable);
        Ok(inner)
    }

    /// Waits until no commit is staged, and returns the store lock.
    fn quiesce(&self) -> Result<RwLockWriteGuard<'_, Inner>, AtlantisError> {
        loop {
            let inner = self.publish_durable()?;
            if inner.pending.is_empty() {
                return Ok(inner);
            }
            let target = inner.staged;
            drop(inner);
            self.durability.wait_synced(target)?;
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
    /// [module docs](crate::mem) for the full protocol.
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
            // Staged commits have records in the active log but are not in
            // the state being copied; rotating now would drop them.
            let mut inner = self.quiesce()?;
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

    /// Number of log syncs so far. With [`SyncMode::Every`] concurrent
    /// commits share syncs, so this grows slower than the commit count.
    pub fn sync_count(&self) -> u64 {
        self.durability.lock().syncs
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
