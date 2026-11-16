//! In-memory HNSW indexes derived from records.
//!
//! A vector index definition lives in the catalog; the HNSW graph itself is
//! derived data. It is built lazily on first search by scanning the records
//! that carry the index's label, and kept current after every commit by
//! re-reading the records the commit changed. Searches see committed data
//! only, not a transaction's own uncommitted writes.
//!
//! Locking: a short-lived mutex guards the map of built indexes; each index
//! has its own read-write lock. Searches of one index run in parallel, and
//! a commit blocks only the indexes of its database while it updates them.
//!
//! Filtered search: a [`FieldFilter`] (pushed down from `YIELD ... WHERE`)
//! is evaluated over a *column*, a per-slot copy of one record field that the
//! index keeps next to the graph. A column is built on first use and kept
//! current by the same per-commit refresh as the vectors; equality and `IN`
//! use a value → slots map, other operators scan the column. The resulting
//! allow set drives
//! [`HnswIndex::search_filtered`](atlantis_vector::HnswIndex::search_filtered).

mod built;
mod column;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use atlantis_core::{AtlantisError, DatabaseRef, NodeId, RecordId, Value};
use atlantis_cypher::ast::BinaryOp;
use atlantis_kv::{Isolation, TxMode};
use atlantis_storage::{RecordStore, VectorIndexDef};
use atlantis_vector::HnswParams;
use built::Built;

/// A nearest-neighbour search request.
#[derive(Debug, Clone)]
pub struct VectorQuery {
    pub vector: Vec<f32>,
    /// Number of results.
    pub k: usize,
    /// Candidate pool size; defaults to the index's `ef_search`.
    pub ef_search: Option<usize>,
    /// Minimum score a hit must reach.
    pub threshold: Option<f64>,
    /// Conditions every hit must meet (a conjunction); empty for none.
    pub filter: Vec<FieldFilter>,
}

/// One condition on a top-level record field: `field op value` (or
/// `value op field` when `field_on_left` is false). Evaluated with the same
/// comparison semantics as `WHERE`.
#[derive(Debug, Clone)]
pub struct FieldFilter {
    pub field: String,
    pub op: BinaryOp,
    pub value: Value,
    pub field_on_left: bool,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorHit {
    pub id: RecordId,
    /// Higher is more similar: cosine similarity, dot product, or
    /// `1 / (1 + distance)` for Euclidean.
    pub score: f64,
}

/// One index slot. `None` until the first search has built it; the builder
/// holds the write lock from before the slot is published until the build
/// is done, so no one sees a half-built index.
type Slot = Arc<RwLock<Option<Built>>>;

fn read(slot: &Slot) -> RwLockReadGuard<'_, Option<Built>> {
    slot.read().unwrap_or_else(|e| e.into_inner())
}

fn write(slot: &Slot) -> RwLockWriteGuard<'_, Option<Built>> {
    slot.write().unwrap_or_else(|e| e.into_inner())
}

/// All built vector indexes of a datastore.
pub struct VectorIndexes {
    defaults: HnswParams,
    /// Threads used to build an index from its records.
    build_threads: usize,
    built: Mutex<HashMap<(DatabaseRef, String), Slot>>,
}

impl VectorIndexes {
    pub fn new(defaults: HnswParams, build_threads: usize) -> Self {
        VectorIndexes {
            defaults,
            build_threads: build_threads.max(1),
            built: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(DatabaseRef, String), Slot>> {
        self.built.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Searches the index for the records nearest to `query.vector`.
    ///
    /// `def` must be the index definition as read in the caller's
    /// transaction; if it differs from the built index (the index was
    /// dropped and recreated), the index is rebuilt.
    pub fn search(
        &self,
        store: &RecordStore,
        db: &DatabaseRef,
        def: &VectorIndexDef,
        query: &VectorQuery,
    ) -> Result<Vec<VectorHit>, AtlantisError> {
        let request = query;
        let query = request.vector.as_slice();
        if query.len() != def.dimension {
            return Err(AtlantisError::DimensionMismatch {
                expected: def.dimension,
                actual: query.len(),
            });
        }
        let key = (db.clone(), def.name.clone());
        loop {
            let slot = self.lock().get(&key).cloned();
            if let Some(slot) = slot {
                let guard = read(&slot);
                match guard.as_ref() {
                    Some(index) if index.def == *def => {
                        let fields: Vec<&str> = request
                            .filter
                            .iter()
                            .map(|f| f.field.as_str())
                            .filter(|f| !index.columns.contains_key(*f))
                            .collect();
                        if fields.is_empty() {
                            return Self::search_built(index, request);
                        }
                        // Build the missing columns under the write lock,
                        // then search again.
                        drop(guard);
                        self.add_columns(store, db, def, &slot, &request.filter)?;
                        continue;
                    }
                    // A failed build left the slot empty; build again.
                    None => {}
                    // Redefined: rebuild below.
                    Some(_) => {}
                }
            }
            if let Some(result) = self.build(store, &key, def)? {
                let guard = read(&result);
                if let Some(index) = guard.as_ref().filter(|i| i.def == *def) {
                    if request.filter.is_empty() {
                        return Self::search_built(index, request);
                    }
                }
            }
            // Another thread replaced the slot meanwhile; look again.
        }
    }

    fn search_built(index: &Built, request: &VectorQuery) -> Result<Vec<VectorHit>, AtlantisError> {
        let (query, k, ef_search) = (request.vector.as_slice(), request.k, request.ef_search);
        let threshold = request.threshold;
        let hits = if request.filter.is_empty() {
            index.hnsw.search(query, k, ef_search)?
        } else {
            let allow = index.allow_set(&request.filter);
            index.hnsw.search_filtered(query, k, ef_search, &allow)?
        };
        Ok(hits
            .into_iter()
            .filter_map(|(slot, distance)| {
                let id = index.records.get(&slot.0)?.clone();
                Some(VectorHit {
                    id,
                    score: index.score(distance),
                })
            })
            .filter(|hit| threshold.is_none_or(|t| hit.score >= t))
            .collect())
    }

    /// Adds the columns `filters` need to the built index in `slot`.
    fn add_columns(
        &self,
        store: &RecordStore,
        db: &DatabaseRef,
        def: &VectorIndexDef,
        slot: &Slot,
        filters: &[FieldFilter],
    ) -> Result<(), AtlantisError> {
        let mut guard = write(slot);
        let Some(index) = guard.as_mut().filter(|i| i.def == *def) else {
            return Ok(());
        };
        let fields: Vec<&str> = filters.iter().map(|f| f.field.as_str()).collect();
        let mut tx = store.begin(db.clone(), TxMode::ReadOnly, Isolation::Snapshot)?;
        index.add_columns(&mut tx, &fields)
    }

    /// Builds the index for `key` from the records, publishing a new slot.
    ///
    /// The new slot is write-locked before it is published (under the map
    /// lock), so a commit applied concurrently waits for the build and then
    /// re-reads the records it changed; a commit that finished before the
    /// build's snapshot is already in it. Returns the slot, or `None` if
    /// another thread published a matching index first.
    fn build(
        &self,
        store: &RecordStore,
        key: &(DatabaseRef, String),
        def: &VectorIndexDef,
    ) -> Result<Option<Slot>, AtlantisError> {
        let slot: Slot = Arc::new(RwLock::new(None));
        let mut guard = write(&slot);
        {
            let mut map = self.lock();
            if let Some(existing) = map.get(key).cloned() {
                // Someone else may have built it while we waited.
                let current = match existing.try_read() {
                    Ok(g) => Some(g.as_ref().is_some_and(|b| b.def == *def)),
                    Err(_) => None,
                };
                match current {
                    Some(true) => return Ok(Some(existing)),
                    // Outdated or failed: replace it below.
                    Some(false) => {}
                    // Being built or updated right now: wait for it.
                    None => {
                        drop(map);
                        drop(guard);
                        let ok = read(&existing).as_ref().is_some_and(|b| b.def == *def);
                        return Ok(ok.then_some(existing));
                    }
                }
            }
            map.insert(key.clone(), slot.clone());
        }
        let mut index = Built::new(def.clone(), &self.defaults);
        let built = (|| {
            let mut tx = store.begin(key.0.clone(), TxMode::ReadOnly, Isolation::Snapshot)?;
            let records = tx.with_label(&def.label)?;
            let mut items = Vec::with_capacity(records.len());
            for record in records {
                if let Some(vector) = index.vector_of(record.get(&def.field)) {
                    let slot = index.next;
                    index.next += 1;
                    items.push((NodeId(slot), vector));
                    index.slots.insert(record.id.to_string(), slot);
                    index.records.insert(slot, record.id);
                }
            }
            index.hnsw.insert_batch(items, self.build_threads)
        })();
        match built {
            Ok(()) => {
                *guard = Some(index);
                drop(guard);
                Ok(Some(slot))
            }
            Err(e) => {
                drop(guard);
                let mut map = self.lock();
                if map.get(key).is_some_and(|s| Arc::ptr_eq(s, &slot)) {
                    map.remove(key);
                }
                Err(e)
            }
        }
    }

    /// Updates built indexes of `db` after a commit changed `changed`.
    pub fn apply(
        &self,
        store: &RecordStore,
        db: &DatabaseRef,
        changed: &[RecordId],
    ) -> Result<(), AtlantisError> {
        if changed.is_empty() {
            return Ok(());
        }
        let slots: Vec<((DatabaseRef, String), Slot)> = self
            .lock()
            .iter()
            .filter(|((d, _), _)| d == db)
            .map(|(k, s)| (k.clone(), s.clone()))
            .collect();
        if slots.is_empty() {
            return Ok(());
        }
        let mut tx = match store.begin(db.clone(), TxMode::ReadOnly, Isolation::Snapshot) {
            Ok(tx) => tx,
            Err(e) => {
                self.forget(&slots);
                return Err(e);
            }
        };
        let mut stale = Vec::new();
        let mut first_err = None;
        for (key, slot) in &slots {
            let mut guard = write(slot);
            let Some(index) = guard.as_mut() else {
                continue;
            };
            let result = changed.iter().try_for_each(|id| index.refresh(&mut tx, id));
            // Stale or degraded indexes are dropped and rebuilt on next search.
            if result.is_err() || index.needs_rebuild() {
                stale.push((key.clone(), slot.clone()));
            }
            if let Err(e) = result {
                first_err.get_or_insert(e);
            }
        }
        self.forget(&stale);
        first_err.map_or(Ok(()), Err)
    }

    /// Drops the given slots from the map, unless they were replaced.
    fn forget(&self, slots: &[((DatabaseRef, String), Slot)]) {
        let mut map = self.lock();
        for (key, slot) in slots {
            if map.get(key).is_some_and(|s| Arc::ptr_eq(s, slot)) {
                map.remove(key);
            }
        }
    }

    /// Forgets a built index (after it was dropped or redefined).
    pub fn invalidate(&self, db: &DatabaseRef, name: &str) {
        self.lock().remove(&(db.clone(), name.to_string()));
    }
}
