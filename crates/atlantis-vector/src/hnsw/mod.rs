//! HNSW (Hierarchical Navigable Small World) vector index.
//!
//! Implements the HNSW algorithm for approximate nearest neighbor search.
//! Builds a multi-layer graph where upper layers are sparse (for fast
//! navigation) and the base layer is dense (for accurate search).
//!
//! Layout, chosen for the search loop:
//!
//! - vectors live in one contiguous arena ([`VectorStore`]);
//! - layer-0 links, which every search walks, are one flat `u32` array with
//!   a fixed stride of `m0 + 1` per node (a count, then the ids), so a
//!   node's links sit next to each other in memory;
//! - upper-layer links (a few percent of nodes) are per-node lists;
//! - the inverse norm of every vector is computed once at insert, so a
//!   cosine distance is one dot product;
//! - visited sets are epoch-stamped arrays reused per thread instead of a
//!   hash set per search.
//!
//! Concurrency: link lists are arrays of atomics. Searches read them without
//! locks; a writer changes a node's list under that node's lock stripe and
//! never holds two stripes at once. [`HnswIndex::insert_batch`] uses this to
//! link many nodes on several threads (as hnswlib does): all vectors, norms
//! and levels are written first, then threads link nodes concurrently. A
//! reader racing a writer can see a list that mixes old and new ids; every
//! id in it is a valid node, so the search stays correct and only follows a
//! slightly different path.
//!
//! Ids are dense slot numbers (`NodeId` below `u32::MAX`); the engine
//! assigns them sequentially.

mod filtered;
mod graph;
mod insert;
mod metric;
mod scored;
mod search;
mod select;
mod visited;

#[cfg(test)]
mod metric_tests;
#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard};

use atlantis_core::{DistanceMetricKind, NodeId};

use crate::vector_store::VectorStore;

/// `levels[i]` of a slot that holds no node.
const NO_LEVEL: u8 = u8::MAX;

/// `entry` of an empty index.
const NO_ENTRY: u64 = u64::MAX;

/// Number of lock stripes guarding link-list writes.
const LOCK_STRIPES: usize = 1024;

fn pack_entry(node: u32, level: usize) -> u64 {
    ((level as u64) << 32) | node as u64
}

fn lock(m: &Mutex<()>) -> MutexGuard<'_, ()> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The HNSW vector index.
pub struct HnswIndex {
    params: crate::params::HnswParams,
    metric: DistanceMetricKind,
    vectors: VectorStore,
    /// Per slot: `1 / ‖v‖` (0 for the zero vector or an empty slot).
    inv_norms: Vec<f32>,
    /// Per slot: the node's top layer, or [`NO_LEVEL`].
    levels: Vec<u8>,
    /// Layer-0 links, stride `m0 + 1`: `[count, id_0, .., id_{m0-1}]`.
    /// Links are **directed**: pruning a node's list never removes the links
    /// other nodes hold to it (undirected pruning disconnected nodes).
    layer0: Vec<AtomicU32>,
    /// Per slot: links at layers `1..=level`, stride `m + 1` per layer,
    /// layer `l` at `(l - 1) * (m + 1)`.
    upper: Vec<Box<[AtomicU32]>>,
    /// Per slot: tombstoned. Deleted nodes stay in the graph as waypoints.
    deleted: Vec<bool>,
    deleted_count: usize,
    /// Entry point (low 32 bits) and top level (high 32 bits), or
    /// [`NO_ENTRY`].
    entry: AtomicU64,
    /// Held for the whole insert of a node that raises the top level.
    top: Mutex<()>,
    /// Link-list write locks; node `i` uses stripe `i % LOCK_STRIPES`.
    stripes: Box<[Mutex<()>]>,
}

impl HnswIndex {
    /// Creates a new HNSW index with the given parameters and distance metric.
    pub fn new(
        params: crate::params::HnswParams,
        dimensions: usize,
        metric: DistanceMetricKind,
    ) -> Self {
        let mut params = params;
        params.m = params.m.max(1);
        params.m0 = params.m0.max(params.m);
        HnswIndex {
            params,
            metric,
            vectors: VectorStore::new(dimensions),
            inv_norms: Vec::new(),
            levels: Vec::new(),
            layer0: Vec::new(),
            upper: Vec::new(),
            deleted: Vec::new(),
            deleted_count: 0,
            entry: AtomicU64::new(NO_ENTRY),
            top: Mutex::new(()),
            stripes: (0..LOCK_STRIPES).map(|_| Mutex::new(())).collect(),
        }
    }

    /// Returns the index parameters.
    pub fn params(&self) -> &crate::params::HnswParams {
        &self.params
    }

    /// Returns the distance metric.
    pub fn metric(&self) -> DistanceMetricKind {
        self.metric
    }

    /// Returns the number of active (non-deleted) vectors.
    pub fn len(&self) -> usize {
        self.vectors.len() - self.deleted_count
    }

    /// Returns `true` if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the vector dimensionality.
    pub fn dimensions(&self) -> usize {
        self.vectors.dimensions()
    }

    /// Returns the entry point node ID, if the index is non-empty.
    pub fn entry_point(&self) -> Option<NodeId> {
        self.entry_state().map(|(e, _)| NodeId(e as u64))
    }

    /// Returns the current max level of the index.
    pub fn max_level(&self) -> usize {
        self.entry_state().map_or(0, |(_, l)| l)
    }

    /// Returns the number of layers the index can have.
    pub fn layer_count(&self) -> usize {
        self.params.max_level + 1
    }

    /// Reserves room for `additional` more slots.
    pub fn reserve(&mut self, additional: usize) {
        let stride = self.params.m0 + 1;
        self.inv_norms.reserve(additional);
        self.levels.reserve(additional);
        self.layer0.reserve(additional * stride);
        self.upper.reserve(additional);
        self.deleted.reserve(additional);
    }

    /// Marks a node as deleted (tombstone).
    ///
    /// The node stays in the graph as a waypoint, so paths through it keep
    /// working; it is never returned and never chosen as a new neighbour.
    pub fn remove(&mut self, node_id: NodeId) {
        if self.vectors.contains(node_id) {
            let d = &mut self.deleted[node_id.0 as usize];
            if !*d {
                *d = true;
                self.deleted_count += 1;
            }
        }
    }

    /// Returns the vector for a given node ID, if it exists.
    pub fn get_vector(&self, node_id: NodeId) -> Option<&[f32]> {
        if self.is_deleted(node_id.0) {
            return None;
        }
        self.vectors.get(node_id)
    }

    fn entry_state(&self) -> Option<(u32, usize)> {
        match self.entry.load(Relaxed) {
            NO_ENTRY => None,
            e => Some((e as u32, (e >> 32) as usize)),
        }
    }

    fn is_deleted(&self, id: u64) -> bool {
        usize::try_from(id)
            .ok()
            .and_then(|i| self.deleted.get(i))
            .copied()
            .unwrap_or(false)
    }
}
