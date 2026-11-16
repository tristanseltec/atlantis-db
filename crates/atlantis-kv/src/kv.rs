//! The transactional key-value interface every storage backend implements.

use std::fmt;

use atlantis_core::AtlantisError;

use crate::keys::KeyRange;

/// A key-value pair returned by a scan.
pub type KvPair = (Vec<u8>, Vec<u8>);

/// Whether a transaction may write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxMode {
    ReadOnly,
    ReadWrite,
}

/// Transaction isolation level (defined in `atlantis-core` so configuration
/// can name it).
pub use atlantis_core::Isolation;

/// A totally ordered commit position. Every commit that writes gets a new,
/// larger versionstamp; change feeds will be keyed by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Versionstamp(pub u64);

impl fmt::Display for Versionstamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:020}", self.0)
    }
}

/// An ordered, transactional key-value store.
pub trait KvStore: Send + Sync {
    /// Starts a transaction reading from a snapshot of the latest commit.
    fn begin(
        &self,
        mode: TxMode,
        isolation: Isolation,
    ) -> Result<Box<dyn KvTransaction>, AtlantisError>;
}

/// One transaction. Writes are buffered and become visible to others only
/// after [`commit`](KvTransaction::commit). Dropping an unfinished
/// transaction cancels it.
pub trait KvTransaction: Send {
    /// Reads a key, seeing this transaction's own writes.
    fn get(&mut self, key: &[u8]) -> Result<Option<Vec<u8>>, AtlantisError>;

    /// Writes a key.
    fn set(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), AtlantisError>;

    /// Writes a key that concurrent transactions may write with the same
    /// bytes, such as a catalog entry created on first use. If a commit
    /// newer than this transaction's snapshot already wrote exactly `value`
    /// to `key`, that is not a conflict (and a read of `key` by a
    /// serializable transaction is not invalidated by it); this write is
    /// then dropped. A different value conflicts as with [`set`](Self::set).
    fn set_idempotent(&mut self, key: Vec<u8>, value: Vec<u8>) -> Result<(), AtlantisError> {
        self.set(key, value)
    }

    /// Deletes a key. Deleting a missing key is not an error.
    fn delete(&mut self, key: &[u8]) -> Result<(), AtlantisError>;

    /// Returns pairs in `range` in ascending key order, at most `limit`.
    fn scan(
        &mut self,
        range: &KeyRange,
        limit: Option<usize>,
    ) -> Result<Vec<KvPair>, AtlantisError>;

    /// Commits, returning the commit's versionstamp. A transaction without
    /// writes returns its snapshot's versionstamp.
    fn commit(&mut self) -> Result<Versionstamp, AtlantisError>;

    /// Discards all buffered writes.
    fn cancel(&mut self) -> Result<(), AtlantisError>;

    /// `true` after commit or cancel.
    fn is_finished(&self) -> bool;

    /// The versionstamp this transaction reads from.
    fn snapshot(&self) -> Versionstamp;
}
