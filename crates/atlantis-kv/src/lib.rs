//! Transactional, ordered key-value layer for Atlantis DB.
//!
//! Every data model (records, edges, indexes, and later full-text, vector
//! and change-feed data) is stored as ordered keys in one key space:
//!
//! - [`encoding`] turns values, record keys and strings into byte strings
//!   that sort in logical order;
//! - [`keys`] defines the key layout (catalog, records, adjacency, labels);
//! - [`kv`] defines the [`KvStore`] / [`KvTransaction`] traits with
//!   snapshot-isolated MVCC transactions;
//! - [`mem`] is the in-memory backend, which can checkpoint to a file.

pub mod encoding;
pub mod keys;
pub mod kv;
pub mod mem;

pub use keys::{EdgeDir, KeyRange};
pub use kv::{Isolation, KvPair, KvStore, KvTransaction, TxMode, Versionstamp};
pub use mem::MemStore;
