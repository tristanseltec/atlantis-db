//! Record and edge store on top of the transactional key-value layer.
//!
//! This is the storage side of the unified record model: documents, graph
//! nodes and relational rows are all [`Record`]s in tables; relationships
//! are [`Edge`] records in edge tables, with adjacency entries on both
//! endpoints so traversals are prefix scans. Tables are created implicitly
//! (schemaless) on first write, or explicitly with
//! [`RecordTx::define_table`].
//!
//! The implementation of [`RecordTx`] is split by concern:
//! `catalog` (namespace, database and table definitions), `crud` (records),
//! `edges` (relationships and adjacency), `labels` (secondary labels),
//! `vector_defs` (vector index definitions) and `codec` (the stored value
//! format).
//!
//! [`Record`]: atlantis_core::Record
//! [`Edge`]: atlantis_core::Edge

use std::collections::HashMap;
use std::sync::Arc;

use atlantis_core::{AtlantisError, DatabaseRef, DistanceMetricKind, RecordId};
use atlantis_kv::keys;
use atlantis_kv::{Isolation, KvStore, KvTransaction, TxMode, Versionstamp};
use serde::{Deserialize, Serialize};

use self::codec::{encode, Stored};

mod catalog;
mod codec;
mod crud;
mod edges;
mod labels;
mod vector_defs;

#[cfg(test)]
mod tests;

/// What a table holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableKind {
    /// Documents, graph nodes or rows.
    Normal,
    /// Relationships. Empty `from` / `to` lists allow any endpoint table.
    Edge { from: Vec<String>, to: Vec<String> },
}

/// A table definition stored in the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableDef {
    pub name: String,
    pub kind: TableKind,
}

impl TableDef {
    pub fn is_edge(&self) -> bool {
        matches!(self.kind, TableKind::Edge { .. })
    }
}

/// A vector index definition stored in the catalog. The index itself is
/// derived data, rebuilt in memory from the records it covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorIndexDef {
    pub name: String,
    /// Records carrying this label (as their table or a secondary label).
    pub label: String,
    /// The field holding the vector.
    pub field: String,
    pub dimension: usize,
    pub metric: DistanceMetricKind,
    pub m: usize,
    pub ef_construction: usize,
}

/// The outcome of a successful commit.
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub versionstamp: Versionstamp,
    /// Records and edges written or deleted by the transaction, each once.
    pub changed: Vec<RecordId>,
}

/// Opens record transactions against a key-value backend.
#[derive(Clone)]
pub struct RecordStore {
    kv: Arc<dyn KvStore>,
}

impl RecordStore {
    pub fn new(kv: Arc<dyn KvStore>) -> Self {
        RecordStore { kv }
    }

    /// Starts a transaction scoped to one database.
    pub fn begin(
        &self,
        db: DatabaseRef,
        mode: TxMode,
        isolation: Isolation,
    ) -> Result<RecordTx, AtlantisError> {
        Ok(RecordTx {
            tx: self.kv.begin(mode, isolation)?,
            db,
            catalog_checked: false,
            changed: HashMap::new(),
        })
    }
}

/// A transaction over records and edges in one database.
///
/// Nothing is visible to other transactions until [`commit`](Self::commit).
/// Dropping the transaction without committing discards its writes.
pub struct RecordTx {
    tx: Box<dyn KvTransaction>,
    db: DatabaseRef,
    catalog_checked: bool,
    /// Changed records, keyed by their encoded storage key.
    changed: HashMap<Vec<u8>, RecordId>,
}

impl RecordTx {
    /// The database this transaction works in.
    pub fn database(&self) -> &DatabaseRef {
        &self.db
    }

    /// Commits all writes atomically.
    pub fn commit(mut self) -> Result<CommitInfo, AtlantisError> {
        let versionstamp = self.tx.commit()?;
        Ok(CommitInfo {
            versionstamp,
            changed: self.changed.into_values().collect(),
        })
    }

    /// `true` if this transaction has written or deleted any record.
    pub fn has_changes(&self) -> bool {
        !self.changed.is_empty()
    }

    fn put_record(
        &mut self,
        id: &RecordId,
        key: Vec<u8>,
        stored: &Stored,
    ) -> Result<(), AtlantisError> {
        // Clone the key only when it is first recorded.
        if !self.changed.contains_key(&key) {
            self.changed.insert(key.clone(), id.clone());
        }
        self.tx.set(key, encode(stored)?)
    }

    fn delete_record_key(&mut self, id: &RecordId) -> Result<(), AtlantisError> {
        let key = keys::record(&self.db, id)?;
        self.tx.delete(&key)?;
        self.changed.entry(key).or_insert_with(|| id.clone());
        Ok(())
    }

    /// Discards all writes.
    pub fn cancel(mut self) -> Result<(), AtlantisError> {
        self.tx.cancel()
    }
}
