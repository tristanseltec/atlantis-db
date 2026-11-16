//! [`Inner`]: the versioned data, open snapshots and staged commits behind
//! a [`MemStore`](super::MemStore), and its MVCC operations.

use std::collections::BTreeMap;

use crate::kv::KvPair;
use crate::wal::{Wal, WalRecord};

use super::{Version, Writes};

#[derive(Default)]
pub(super) struct Inner {
    pub(super) data: BTreeMap<Vec<u8>, Vec<Version>>,
    /// Version of the latest commit.
    pub(super) version: u64,
    /// Open snapshots: snapshot version → number of transactions using it.
    pub(super) active: BTreeMap<u64, usize>,
    /// Write-ahead log; `Some` only for stores opened with
    /// [`MemStore::open_durable`](super::MemStore::open_durable).
    pub(super) wal: Option<Wal>,
    /// Version of the latest staged commit (`>= version`).
    pub(super) staged: u64,
    /// Staged commits waiting for their log record to be synced, by version.
    pub(super) pending: BTreeMap<u64, Writes>,
    /// Key → version of the staged commit that writes it. A key has at most
    /// one: a second writer would have a snapshot below the first one's
    /// version and fail validation.
    pub(super) pending_keys: BTreeMap<Vec<u8>, u64>,
}

impl Inner {
    #[inline]
    pub(super) fn latest_version(&self, key: &[u8]) -> Option<u64> {
        self.data
            .get(key)
            .and_then(|versions| versions.last())
            .map(|(v, _)| *v)
    }

    #[inline]
    pub(super) fn read_at(&self, key: &[u8], snapshot: u64) -> Option<&Vec<u8>> {
        let versions = self.data.get(key)?;
        versions
            .iter()
            .rev()
            .find(|(v, _)| *v <= snapshot)?
            .1
            .as_ref()
    }

    /// Oldest version any open transaction may still read.
    #[inline]
    pub(super) fn horizon(&self) -> u64 {
        self.active.keys().next().copied().unwrap_or(self.version)
    }

    /// Drops versions of `key` that no snapshot at or after `horizon` can see.
    pub(super) fn prune(&mut self, key: &[u8], horizon: u64) {
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
    pub(super) fn latest_pairs(&self) -> Vec<KvPair> {
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
    pub(super) fn apply_replayed(&mut self, record: WalRecord) {
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

    /// Applies one commit's writes at `version` and makes it visible.
    pub(super) fn publish(&mut self, version: u64, writes: Writes) {
        debug_assert_eq!(version, self.version + 1);
        self.version = version;
        let horizon = self.horizon();
        for (key, value) in writes {
            if self.pending_keys.get(&key) == Some(&version) {
                self.pending_keys.remove(&key);
            }
            self.data
                .entry(key.clone())
                .or_default()
                .push((version, value));
            self.prune(&key, horizon);
        }
    }

    /// Publishes staged commits, in order, up to `durable`.
    pub(super) fn publish_durable(&mut self, durable: u64) {
        while let Some(entry) = self.pending.first_entry() {
            let version = *entry.key();
            if version > durable || version != self.version + 1 {
                break;
            }
            let writes = entry.remove();
            self.publish(version, writes);
        }
    }

    /// The newest value of `key` committed or staged after `snapshot`, if
    /// any (`Some(None)`: deleted).
    #[inline]
    pub(super) fn newer_than(&self, key: &[u8], snapshot: u64) -> Option<Option<&Vec<u8>>> {
        if let Some(v) = self.pending_keys.get(key) {
            return self
                .pending
                .get(v)
                .and_then(|w| w.get(key))
                .map(Option::as_ref);
        }
        let (version, value) = self.data.get(key)?.last()?;
        (*version > snapshot).then_some(value.as_ref())
    }

    pub(super) fn release(&mut self, snapshot: u64) {
        if let Some(count) = self.active.get_mut(&snapshot) {
            *count -= 1;
            if *count == 0 {
                self.active.remove(&snapshot);
            }
        }
    }
}
