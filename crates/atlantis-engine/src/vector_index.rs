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

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};

use atlantis_core::{AtlantisError, DatabaseRef, DistanceMetricKind, NodeId, RecordId, Value};
use atlantis_kv::{Isolation, TxMode};
use atlantis_storage::{RecordStore, RecordTx, VectorIndexDef};
use atlantis_vector::{HnswIndex, HnswParams};

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
}

/// One search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorHit {
    pub id: RecordId,
    /// Higher is more similar: cosine similarity, dot product, or
    /// `1 / (1 + distance)` for Euclidean.
    pub score: f64,
}

struct Built {
    def: VectorIndexDef,
    hnsw: HnswIndex,
    /// Internal id → record id.
    records: HashMap<u64, RecordId>,
    /// Record id text → internal id of its current vector.
    slots: HashMap<String, u64>,
    next: u64,
    /// Removed entries still present as HNSW tombstones.
    deleted: usize,
}

impl Built {
    fn new(def: VectorIndexDef, defaults: &HnswParams) -> Self {
        let params = HnswParams {
            m: def.m,
            m0: def.m * 2,
            ef_construction: def.ef_construction,
            ..defaults.clone()
        };
        let hnsw = HnswIndex::new(params, def.dimension, def.metric);
        Built {
            def,
            hnsw,
            records: HashMap::new(),
            slots: HashMap::new(),
            next: 0,
            deleted: 0,
        }
    }

    fn vector_of(&self, value: Option<&Value>) -> Option<Vec<f32>> {
        let v: Vec<f32> = match value? {
            Value::Vector(v) => v.clone(),
            Value::Array(items) => items
                .iter()
                .map(|x| x.as_float().map(|f| f as f32))
                .collect::<Option<_>>()?,
            _ => return None,
        };
        (v.len() == self.def.dimension).then_some(v)
    }

    fn remove(&mut self, id: &RecordId) {
        if let Some(slot) = self.slots.remove(&id.to_string()) {
            self.hnsw.remove(NodeId(slot));
            self.records.remove(&slot);
            self.deleted += 1;
        }
    }

    /// Re-reads one record and updates its entry. An unchanged vector is
    /// left in place, so writes to other fields do not churn the graph.
    fn refresh(&mut self, tx: &mut RecordTx, id: &RecordId) -> Result<(), AtlantisError> {
        let vector = match tx.get(id)? {
            Some(record) if record.has_label(&self.def.label) => {
                self.vector_of(record.get(&self.def.field))
            }
            _ => None,
        };
        if let (Some(v), Some(slot)) = (&vector, self.slots.get(&id.to_string())) {
            if self.hnsw.get_vector(NodeId(*slot)) == Some(v.as_slice()) {
                return Ok(());
            }
        }
        self.remove(id);
        if let Some(vector) = vector {
            let slot = self.next;
            self.next += 1;
            self.hnsw.insert(NodeId(slot), vector)?;
            self.records.insert(slot, id.clone());
            self.slots.insert(id.to_string(), slot);
        }
        Ok(())
    }

    /// `true` once tombstones outnumber live entries; searching such a
    /// graph loses recall, so it is rebuilt.
    fn needs_rebuild(&self) -> bool {
        self.deleted > 1024.max(self.slots.len())
    }

    fn score(&self, distance: f32) -> f64 {
        let d = distance as f64;
        match self.def.metric {
            DistanceMetricKind::Cosine => 1.0 - d,
            DistanceMetricKind::DotProduct => -d,
            DistanceMetricKind::Euclidean => 1.0 / (1.0 + d),
        }
    }
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
        let (k, ef_search, threshold) = (query.k, query.ef_search, query.threshold);
        let query = query.vector.as_slice();
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
                        return Self::search_built(index, query, k, ef_search, threshold);
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
                    return Self::search_built(index, query, k, ef_search, threshold);
                }
            }
            // Another thread replaced the slot meanwhile; look again.
        }
    }

    fn search_built(
        index: &Built,
        query: &[f32],
        k: usize,
        ef_search: Option<usize>,
        threshold: Option<f64>,
    ) -> Result<Vec<VectorHit>, AtlantisError> {
        let hits = index.hnsw.search(query, k, ef_search)?;
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
