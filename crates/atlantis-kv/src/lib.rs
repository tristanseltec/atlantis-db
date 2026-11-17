//! Transactional, ordered key-value layer for Atlantis DB.
//!
//! Every data model (records, edges, indexes, and later full-text, vector
//! and change-feed data) is stored as ordered keys in one key space:
//!
//! - [`encoding`] turns values, record keys and strings into byte strings
//!   that sort in logical order;
//! - [`index_encoding`] is the canonical value encoding of secondary index
//!   keys, where `1` and `1.0` share a key;
//! - [`keys`] defines the key layout (catalog, records, adjacency, labels);
//! - [`kv`] defines the [`KvStore`] / [`KvTransaction`] traits with
//!   snapshot-isolated MVCC transactions;
//! - [`mem`] is the in-memory MVCC backend. It can checkpoint to a file, or
//!   be opened durably ([`MemStore::open_durable`]) so that every commit is
//!   appended to a write-ahead log before it is acknowledged and recovered
//!   on reopen;
//! - [`wal`] defines the write-ahead log format and [`SyncMode`].

pub mod encoding;
pub mod index_encoding;
pub mod keys;
pub mod kv;
pub mod mem;
pub mod wal;

pub use keys::{EdgeDir, KeyRange};
pub use kv::{Isolation, KvPair, KvStore, KvTransaction, TxMode, Versionstamp};
pub use mem::MemStore;
pub use wal::SyncMode;
