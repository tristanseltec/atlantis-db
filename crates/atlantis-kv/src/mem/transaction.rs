//! [`MemTransaction`]: snapshot reads, buffered writes, commit-time
//! conflict validation and staging, and the [`KvTransaction`] impl.

use std::collections::{BTreeMap, HashSet};
use std::ops::Bound;

use atlantis_core::AtlantisError;

use crate::keys::KeyRange;
use crate::kv::{Isolation, KvPair, KvStore, KvTransaction, TxMode, Versionstamp};
use crate::wal::{self, SyncMode};

use super::inner::Inner;
use super::store::MemStore;
use super::Writes;

impl KvStore for MemStore {
    fn begin(
        &self,
        mode: TxMode,
        isolation: Isolation,
    ) -> Result<Box<dyn KvTransaction>, AtlantisError> {
        Ok(Box::new(self.begin_tx(mode, isolation)?))
    }
}

impl MemStore {
    pub(super) fn begin_tx(
        &self,
        mode: TxMode,
        isolation: Isolation,
    ) -> Result<MemTransaction, AtlantisError> {
        let snapshot = {
            let mut inner = self.write()?;
            let snapshot = inner.version;
            *inner.active.entry(snapshot).or_insert(0) += 1;
            snapshot
        };
        Ok(MemTransaction {
            store: self.clone(),
            snapshot,
            mode,
            isolation,
            writes: BTreeMap::new(),
            reads: HashSet::new(),
            scans: Vec::new(),
            idempotent: HashSet::new(),
            finished: false,
        })
    }
}

/// A transaction on a [`MemStore`]: a snapshot plus buffered writes and,
/// for serializable transactions, the reads and scans to validate.
pub(super) struct MemTransaction {
    store: MemStore,
    snapshot: u64,
    mode: TxMode,
    isolation: Isolation,
    writes: Writes,
    /// Keys read (tracked only for serializable transactions).
    reads: HashSet<Vec<u8>>,
    /// Ranges scanned (tracked only for serializable transactions).
    scans: Vec<KeyRange>,
    /// Keys written with `set_idempotent`.
    idempotent: HashSet<Vec<u8>>,
    finished: bool,
}

impl MemTransaction {
    #[inline]
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

    #[inline]
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
            self.idempotent.clear();
        }
    }

    /// Validates, takes the next version and appends the log record, under
    /// the store lock. Returns the version and whether the commit must wait
    /// for a sync before it is published; otherwise it is published already.
    /// The transaction is finished either way.
    pub(super) fn stage(&mut self, store: &MemStore) -> Result<(u64, bool), AtlantisError> {
        let mut inner = store.write()?;
        if let Some(reason) = self.conflict(&inner) {
            drop(inner);
            self.finish();
            return Err(AtlantisError::TxConflict(reason));
        }
        // Idempotent writes a newer commit already made are dropped.
        let redundant: Vec<Vec<u8>> = self
            .idempotent
            .iter()
            .filter(|k| self.redundant(&inner, k))
            .cloned()
            .collect();
        for key in redundant {
            self.writes.remove(&key);
        }
        if self.writes.is_empty() {
            let snapshot = self.snapshot;
            drop(inner);
            self.finish();
            return Ok((snapshot, false));
        }
        let version = inner.staged + 1;
        let mut wait = false;
        if let Some(wal) = inner.wal.as_mut() {
            let logged = match store.durability.failure() {
                Some(e) => {
                    wal.mark_broken();
                    Err(AtlantisError::Storage(e))
                }
                None => wal::encode_record(version, &self.writes).and_then(|f| wal.append(&f)),
            };
            if let Err(e) = logged {
                drop(inner);
                self.finish();
                return Err(e);
            }
            wait = wal.sync_mode() == SyncMode::Every;
            store.durability.note_appended(version);
        }
        inner.staged = version;
        let writes = std::mem::take(&mut self.writes);
        // Release our snapshot before publishing so it does not hold back
        // the pruning horizon.
        inner.release(self.snapshot);
        self.finished = true;
        if wait {
            for key in writes.keys() {
                inner.pending_keys.insert(key.clone(), version);
            }
            inner.pending.insert(version, writes);
        } else {
            inner.publish(version, writes);
        }
        Ok((version, wait))
    }

    /// `true` if `key` was written with `set_idempotent` and a newer commit
    /// (visible or staged) already holds exactly the value this one writes.
    fn redundant(&self, inner: &Inner, key: &[u8]) -> bool {
        self.idempotent.contains(key)
            && match (inner.newer_than(key, self.snapshot), self.writes.get(key)) {
                (Some(theirs), Some(ours)) => theirs == ours.as_ref(),
                _ => false,
            }
    }

    /// Returns a conflict description if a concurrent commit invalidates
    /// this transaction.
    fn conflict(&self, inner: &Inner) -> Option<String> {
        // Staged commits count: they are newer than every open snapshot.
        // A matching idempotent write does not.
        let changed = |key: &[u8]| {
            (inner.latest_version(key).is_some_and(|v| v > self.snapshot)
                || inner.pending_keys.contains_key(key))
                && !self.redundant(inner, key)
        };
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
                    .range::<Vec<u8>, _>(bounds.clone())
                    .any(|(_, versions)| versions.last().is_some_and(|(v, _)| *v > self.snapshot))
                    || inner
                        .pending_keys
                        .range::<Vec<u8>, _>(bounds)
                        .next()
                        .is_some()
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
        self.idempotent.remove(&key);
        self.writes.insert(key, Some(value));
        Ok(())
    }

    fn set_idempotent(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), AtlantisError> {
        self.check_writable()?;
        self.idempotent.insert(key.clone());
        self.writes.insert(key, Some(value));
        Ok(())
    }

    fn delete(&mut self, key: &[u8]) -> Result<(), AtlantisError> {
        self.check_writable()?;
        self.idempotent.remove(key);
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
            inner
                .data
                .range::<Vec<u8>, _>((
                    Bound::Included(range.start.clone()),
                    Bound::Excluded(range.end.clone()),
                ))
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
        let version = self.stage(&store)?;
        if version.1 {
            store.durability.wait_synced(version.0)?;
            drop(store.publish_durable()?);
        }
        Ok(Versionstamp(version.0))
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
