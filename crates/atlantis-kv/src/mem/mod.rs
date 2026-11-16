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
//! - **Commit (group commit).** Under the store-wide write lock a commit is
//!   *staged*: it is validated against both visible and staged commits,
//!   takes the next version, and appends one log record with its version
//!   and writes. The lock is then released. With [`SyncMode::Every`] the
//!   commit waits, without the lock, until the log is synced past its
//!   record: the first waiter that finds no sync running becomes the
//!   leader and syncs everything appended so far, so concurrent commits
//!   share one `fdatasync`. Once durable, staged commits are *published*
//!   (applied in memory and made visible) strictly in version order, and
//!   the commit is acknowledged. Readers never see a commit that a crash
//!   could lose, and never wait for a sync. With [`SyncMode::Interval`]
//!   and [`SyncMode::Never`] a commit is published right after its append;
//!   the interval mode syncs on a background thread. If the append fails
//!   the commit fails, nothing is applied and its snapshot is released; if
//!   a sync fails the log is marked broken and every staged commit fails
//!   (its record may or may not be on disk). Commits without writes log
//!   nothing; cancelled or conflicting transactions never reach the log.
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
//!
//! [`SyncMode::Every`]: crate::wal::SyncMode::Every
//! [`SyncMode::Interval`]: crate::wal::SyncMode::Interval
//! [`SyncMode::Never`]: crate::wal::SyncMode::Never

mod checkpoint;
mod durability;
mod inner;
mod store;
mod transaction;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

pub use store::MemStore;

/// One committed version: the commit version and the value, or `None` for
/// a deletion.
type Version = (u64, Option<Vec<u8>>);

/// A transaction's buffered writes (`None` deletes).
type Writes = BTreeMap<Vec<u8>, Option<Vec<u8>>>;
