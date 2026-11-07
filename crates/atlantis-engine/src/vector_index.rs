//! In-memory HNSW indexes derived from records.
//!
//! A vector index definition lives in the catalog; the HNSW graph itself is
//! derived data. It is built lazily on first search by scanning the records
//! that carry the index's label, and kept current after every commit by
//! re-reading the records the commit changed. Searches see committed data
//! only, not a transaction's own uncommitted writes.

use std::collections::HashMap;
use std::sync::Mutex;

use atlantis_core::{AtlantisError, DatabaseRef, DistanceMetricKind, NodeId, RecordId, Value};
use atlantis_kv::{Isolation, TxMode};
use atlantis_storage::{RecordStore, RecordTx, VectorIndexDef};
use atlantis_vector::{distance::from_kind, HnswIndex, HnswParams};

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
        let hnsw = HnswIndex::new(params, def.dimension, from_kind(def.metric));
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

/// All built vector indexes of a datastore.
pub struct VectorIndexes {
    defaults: HnswParams,
    built: Mutex<HashMap<(DatabaseRef, String), Built>>,
}

impl VectorIndexes {
    pub fn new(defaults: HnswParams) -> Self {
        VectorIndexes {
            defaults,
            built: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(DatabaseRef, String), Built>> {
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
        let mut built = self.lock();
        let key = (db.clone(), def.name.clone());
        if built.get(&key).is_none_or(|b| b.def != *def) {
            // Build while holding the lock, so commits applied concurrently
            // wait and then re-read the latest state.
            let mut index = Built::new(def.clone(), &self.defaults);
            let mut tx = store.begin(db.clone(), TxMode::ReadOnly, Isolation::Snapshot)?;
            for record in tx.with_label(&def.label)? {
                if let Some(vector) = index.vector_of(record.get(&def.field)) {
                    let slot = index.next;
                    index.next += 1;
                    index.hnsw.insert(NodeId(slot), vector)?;
                    index.slots.insert(record.id.to_string(), slot);
                    index.records.insert(slot, record.id);
                }
            }
            built.insert(key.clone(), index);
        }
        let index = built.get(&key).expect("just built");
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
        let mut built = self.lock();
        if !built.keys().any(|(d, _)| d == db) {
            return Ok(());
        }
        let result = (|| {
            let mut tx = store.begin(db.clone(), TxMode::ReadOnly, Isolation::Snapshot)?;
            for ((d, _), index) in built.iter_mut() {
                if d != db {
                    continue;
                }
                for id in changed {
                    index.refresh(&mut tx, id)?;
                }
            }
            Ok::<(), AtlantisError>(())
        })();
        // Stale or degraded indexes are dropped and rebuilt on next search.
        match result {
            Ok(()) => built.retain(|(d, _), index| d != db || !index.needs_rebuild()),
            Err(_) => built.retain(|(d, _), _| d != db),
        }
        result
    }

    /// Forgets a built index (after it was dropped or redefined).
    pub fn invalidate(&self, db: &DatabaseRef, name: &str) {
        self.lock().remove(&(db.clone(), name.to_string()));
    }
}
