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

use std::cell::RefCell;
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashSet};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard};

use rand::Rng;

use atlantis_core::{AtlantisError, DistanceMetricKind, NodeId};

use crate::distance::{cosine_from_dot, dot, inverse_norm, l2_squared};
use crate::params::SearchParams;
use crate::vector_store::VectorStore;

/// `levels[i]` of a slot that holds no node.
const NO_LEVEL: u8 = u8::MAX;

/// `entry` of an empty index.
const NO_ENTRY: u64 = u64::MAX;

/// Number of lock stripes guarding link-list writes.
const LOCK_STRIPES: usize = 1024;

/// Nodes a batch links on one thread before going parallel: threads that
/// all insert into a tiny graph contend on the same lists and pick poorer
/// neighbours.
const SEQUENTIAL_PREFIX: usize = 1024;

fn pack_entry(node: u32, level: usize) -> u64 {
    ((level as u64) << 32) | node as u64
}

fn lock(m: &Mutex<()>) -> MutexGuard<'_, ()> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A node and its distance to the current query, ordered by distance
/// (then id, so equal distances order deterministically).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Scored {
    dist: f32,
    id: u32,
}

impl Eq for Scored {}

impl PartialOrd for Scored {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Scored {
    fn cmp(&self, other: &Self) -> Ordering {
        self.dist
            .total_cmp(&other.dist)
            .then(self.id.cmp(&other.id))
    }
}

/// A query vector with its inverse norm, computed once per search.
struct Query<'a> {
    v: &'a [f32],
    inv_norm: f32,
}

/// Visited marks for one search: slot `i` was visited iff
/// `marks[i] == epoch`. Starting a search bumps the epoch instead of
/// clearing the array.
#[derive(Default)]
struct Visited {
    marks: Vec<u32>,
    epoch: u32,
}

impl Visited {
    fn start(&mut self, slots: usize) {
        if self.marks.len() < slots {
            self.marks.resize(slots, 0);
        }
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.marks.fill(0);
            self.epoch = 1;
        }
    }

    /// Marks `i`; `true` if it was not marked yet.
    #[inline]
    fn insert(&mut self, i: u32) -> bool {
        let m = &mut self.marks[i as usize];
        if *m == self.epoch {
            false
        } else {
            *m = self.epoch;
            true
        }
    }
}

thread_local! {
    static VISITED: RefCell<Visited> = RefCell::new(Visited::default());
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

    /// The distance between two vectors under this index's metric, as
    /// reported by [`search`](Self::search).
    pub fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        let d = match self.metric {
            DistanceMetricKind::Cosine => {
                cosine_from_dot(dot(a, b), inverse_norm(a), inverse_norm(b))
            }
            DistanceMetricKind::Euclidean => l2_squared(a, b),
            DistanceMetricKind::DotProduct => -dot(a, b),
        };
        self.reported(d)
    }

    /// Inserts a vector into the index with the given node ID.
    ///
    /// Follows Malkov & Yashunin (2018), Algorithms 1 and 4: descend greedily
    /// to the node's level, then at each layer down to 0 search with
    /// `ef_construction`, link to `m` neighbours chosen by the diversity
    /// heuristic, and add the reverse links, shrinking a neighbour's list
    /// with the same heuristic when it exceeds its cap (`m0` at layer 0, `m`
    /// above).
    pub fn insert(&mut self, node_id: NodeId, vector: Vec<f32>) -> Result<(), AtlantisError> {
        self.check_new(node_id, &vector)?;
        let idx = self.prepare(node_id, &vector);
        self.link(idx);
        Ok(())
    }

    /// Inserts many vectors, linking them on up to `threads` threads.
    ///
    /// Every item is validated first (dimensions, id range, ids new and
    /// distinct); on error nothing is inserted. The result is a graph of the
    /// same quality as inserting the items one by one, in a different order.
    pub fn insert_batch(
        &mut self,
        items: Vec<(NodeId, Vec<f32>)>,
        threads: usize,
    ) -> Result<(), AtlantisError> {
        let mut seen = HashSet::with_capacity(items.len());
        for (id, v) in &items {
            self.check_new(*id, v)?;
            if !seen.insert(id.0) {
                return Err(AtlantisError::InvalidValue(format!(
                    "HNSW node {} appears twice in one batch",
                    id.0
                )));
            }
        }
        drop(seen);
        if let Some(max) = items.iter().map(|(id, _)| id.0 as usize).max() {
            self.reserve((max + 1).saturating_sub(self.levels.len()));
            self.ensure_slot(max);
        }
        let ids: Vec<u32> = items
            .into_iter()
            .map(|(id, v)| self.prepare(id, &v))
            .collect();

        let warmup = if threads <= 1 {
            ids.len()
        } else {
            SEQUENTIAL_PREFIX
                .saturating_sub(self.len() - ids.len())
                .min(ids.len())
        };
        for &idx in &ids[..warmup] {
            self.link(idx);
        }
        let rest = &ids[warmup..];
        if rest.is_empty() {
            return Ok(());
        }
        let next = AtomicUsize::new(0);
        let this = &*self;
        std::thread::scope(|scope| {
            for _ in 0..threads.min(rest.len()) {
                scope.spawn(|| loop {
                    let i = next.fetch_add(1, Relaxed);
                    let Some(&idx) = rest.get(i) else { break };
                    this.link(idx);
                });
            }
        });
        Ok(())
    }

    /// Searches for the k nearest neighbors of the query vector.
    ///
    /// Returns a sorted list of `(NodeId, f32)` where the float is the distance
    /// (lower = more similar).
    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        ef_search: Option<usize>,
    ) -> Result<Vec<(NodeId, f32)>, AtlantisError> {
        if query.len() != self.vectors.dimensions() {
            return Err(AtlantisError::DimensionMismatch {
                expected: self.vectors.dimensions(),
                actual: query.len(),
            });
        }
        let Some((entry, max_level)) = self.entry_state() else {
            return Ok(Vec::new());
        };
        let ef = ef_search.unwrap_or(self.params.ef_search).max(k);
        let query = Query {
            v: query,
            inv_norm: inverse_norm(query),
        };

        let mut entry_points = vec![Scored {
            dist: self.dist_to(&query, entry),
            id: entry,
        }];
        for l in (1..=max_level).rev() {
            entry_points = self.search_layer(&query, &entry_points, 1, l, true);
        }
        let mut results: Vec<(NodeId, f32)> = self
            .search_layer(&query, &entry_points, ef, 0, false)
            .into_iter()
            .map(|c| (NodeId(c.id as u64), self.reported(c.dist)))
            .collect();
        results.truncate(k);
        Ok(results)
    }

    /// Searches with score threshold filtering.
    pub fn search_with_params(
        &self,
        query: &[f32],
        params: &SearchParams,
    ) -> Result<Vec<(NodeId, f32)>, AtlantisError> {
        let mut results = self.search(query, params.k, params.ef_search)?;

        if let Some(threshold) = params.score_threshold {
            // For cosine distance, similarity = 1 - distance
            // threshold is similarity, so keep results where 1 - distance >= threshold
            results.retain(|(_, dist)| 1.0 - dist >= threshold);
        }

        Ok(results)
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

    // ---- Internal methods ----

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

    /// Rejects a vector that cannot be inserted under `node_id`.
    fn check_new(&self, node_id: NodeId, vector: &[f32]) -> Result<(), AtlantisError> {
        if vector.len() != self.vectors.dimensions() {
            return Err(AtlantisError::DimensionMismatch {
                expected: self.vectors.dimensions(),
                actual: vector.len(),
            });
        }
        if node_id.0 >= u32::MAX as u64 {
            return Err(AtlantisError::InvalidValue(format!(
                "HNSW node id {} is too large (ids are dense slots below {})",
                node_id.0,
                u32::MAX
            )));
        }
        if self.vectors.contains(node_id) {
            // Links were chosen for the old vector, and other nodes link to
            // this id; replacing it in place would leave a wrong graph.
            return Err(AtlantisError::InvalidValue(format!(
                "HNSW node {} already exists; insert the new vector under a new id",
                node_id.0
            )));
        }
        Ok(())
    }

    /// Stores a checked vector with its norm, level and empty link lists.
    /// The node is not reachable until [`link`](Self::link) runs.
    fn prepare(&mut self, node_id: NodeId, vector: &[f32]) -> u32 {
        let idx = node_id.0 as usize;
        self.ensure_slot(idx);
        self.vectors.write(idx, vector);
        self.inv_norms[idx] = inverse_norm(vector);
        let level = self.random_level();
        self.levels[idx] = level as u8;
        self.upper[idx] = (0..level * (self.params.m + 1))
            .map(|_| AtomicU32::new(0))
            .collect();
        idx as u32
    }

    fn ensure_slot(&mut self, idx: usize) {
        if idx < self.levels.len() {
            return;
        }
        let n = idx + 1;
        self.inv_norms.resize(n, 0.0);
        self.levels.resize(n, NO_LEVEL);
        self.layer0
            .resize_with(n * (self.params.m0 + 1), || AtomicU32::new(0));
        self.upper.resize_with(n, || Vec::new().into_boxed_slice());
        self.deleted.resize(n, false);
    }

    /// Assigns a random level using the HNSW formula:
    /// `level = floor(-ln(unif(0,1)) * mL)` where `mL = 1 / ln(M)`.
    fn random_level(&self) -> usize {
        let ml = 1.0 / (self.params.m.max(2) as f64).ln();
        let uniform: f64 = rand::thread_rng().gen_range(f64::EPSILON..1.0);
        let level = (-uniform.ln() * ml).floor() as usize;
        level.min(self.params.max_level).min(NO_LEVEL as usize - 1)
    }

    /// Links a prepared node into the graph. Takes `&self`: several threads
    /// may link different nodes at once.
    fn link(&self, idx: u32) {
        let level = self.levels[idx as usize] as usize;
        let mut state = self.entry_state();
        // A node that becomes the new top holds `top` for its whole insert,
        // so two such nodes do not race to replace the entry point.
        let mut top_guard = None;
        if state.is_none_or(|(_, max)| level > max) {
            top_guard = Some(lock(&self.top));
            state = self.entry_state();
            if state.is_some_and(|(_, max)| level <= max) {
                top_guard = None;
            }
        }
        let Some((entry, max_level)) = state else {
            self.entry.store(pack_entry(idx, level), Relaxed);
            return;
        };

        let query = Query {
            v: self.vectors.slot(idx as usize),
            inv_norm: self.inv_norms[idx as usize],
        };
        let mut entry_points = vec![Scored {
            dist: self.dist_to(&query, entry),
            id: entry,
        }];
        for l in (level + 1..=max_level).rev() {
            entry_points = self.search_layer(&query, &entry_points, 1, l, true);
        }
        for l in (0..=level.min(max_level)).rev() {
            let found =
                self.search_layer(&query, &entry_points, self.params.ef_construction, l, true);
            let candidates: Vec<Scored> = found
                .iter()
                .copied()
                .filter(|c| c.id != idx && !self.deleted[c.id as usize])
                .collect();
            let neighbors = self.select_neighbors(&candidates, self.params.m);
            let cap = self.cap(l);
            self.merge_links(idx, l, &neighbors, cap);
            for &neighbor in &neighbors {
                self.merge_links(neighbor, l, &[idx], cap);
            }
            if !found.is_empty() {
                entry_points = found;
            }
        }
        if level > max_level {
            self.entry.store(pack_entry(idx, level), Relaxed);
        }
        drop(top_guard);
    }

    fn cap(&self, layer: usize) -> usize {
        if layer == 0 {
            self.params.m0
        } else {
            self.params.m
        }
    }

    /// The link slots of `node` at `layer`: the count, then `cap` ids.
    fn link_slots(&self, node: u32, layer: usize) -> &[AtomicU32] {
        let i = node as usize;
        match self.levels.get(i) {
            Some(&lvl) if lvl != NO_LEVEL && layer <= lvl as usize => {}
            _ => return &[],
        }
        if layer == 0 {
            let stride = self.params.m0 + 1;
            &self.layer0[i * stride..(i + 1) * stride]
        } else {
            let stride = self.params.m + 1;
            &self.upper[i][(layer - 1) * stride..layer * stride]
        }
    }

    /// The links of `node` at `layer`, as atomics to load.
    #[inline]
    fn links(&self, node: u32, layer: usize) -> &[AtomicU32] {
        let slots = self.link_slots(node, layer);
        let Some((count, ids)) = slots.split_first() else {
            return &[];
        };
        let n = (count.load(Relaxed) as usize).min(ids.len());
        &ids[..n]
    }

    fn link_ids(&self, node: u32, layer: usize) -> Vec<u32> {
        self.links(node, layer)
            .iter()
            .map(|a| a.load(Relaxed))
            .collect()
    }

    /// Writes `node`'s list at `layer`. The caller holds `node`'s stripe.
    fn store_links(&self, node: u32, layer: usize, links: &[u32]) {
        let slots = self.link_slots(node, layer);
        let Some((count, ids)) = slots.split_first() else {
            return;
        };
        let n = links.len().min(ids.len());
        for (slot, &id) in ids.iter().zip(&links[..n]) {
            slot.store(id, Relaxed);
        }
        count.store(n as u32, Relaxed);
    }

    /// Adds `new` links to `node`'s list at `layer`, shrinking it to `cap`
    /// with the heuristic when it overflows. Only `node`'s list changes, and
    /// links that other threads added meanwhile are kept as candidates.
    fn merge_links(&self, node: u32, layer: usize, new: &[u32], cap: usize) {
        let _guard = lock(&self.stripes[node as usize % LOCK_STRIPES]);
        let mut list = self.link_ids(node, layer);
        let before = list.len();
        for &n in new {
            if n != node && !list.contains(&n) {
                list.push(n);
            }
        }
        if list.len() == before {
            return;
        }
        if list.len() <= cap {
            self.store_links(node, layer, &list);
            return;
        }
        let mut scored: Vec<Scored> = list
            .into_iter()
            .filter(|&n| !self.deleted[n as usize])
            .map(|n| Scored {
                dist: self.dist_between(node, n),
                id: n,
            })
            .collect();
        scored.sort_unstable();
        let kept = self.select_neighbors(&scored, cap);
        self.store_links(node, layer, &kept);
    }

    /// Best-first search of one layer from `entry_points`, keeping the `ef`
    /// closest nodes found (Algorithm 2). The result is sorted, closest first.
    ///
    /// Deleted nodes are always traversed. With `include_deleted` they are
    /// also returned, which the descent through upper layers and insertion
    /// need: a deleted entry point must still hand the search down a layer.
    /// The final layer-0 search of a query passes `false`, so tombstones do
    /// not take result slots.
    fn search_layer(
        &self,
        query: &Query<'_>,
        entry_points: &[Scored],
        ef: usize,
        layer: usize,
        include_deleted: bool,
    ) -> Vec<Scored> {
        VISITED.with(|visited| {
            let mut visited = visited.borrow_mut();
            visited.start(self.levels.len());
            self.search_layer_with(
                &mut visited,
                query,
                entry_points,
                ef,
                layer,
                include_deleted,
            )
        })
    }

    fn search_layer_with(
        &self,
        visited: &mut Visited,
        query: &Query<'_>,
        entry_points: &[Scored],
        ef: usize,
        layer: usize,
        include_deleted: bool,
    ) -> Vec<Scored> {
        let ef = ef.max(1);
        // Min-heap of nodes to expand; max-heap of the best `ef` found.
        let mut candidates: BinaryHeap<Reverse<Scored>> = BinaryHeap::with_capacity(ef * 2);
        let mut results: BinaryHeap<Scored> = BinaryHeap::with_capacity(ef + 1);

        for &ep in entry_points {
            if visited.insert(ep.id) {
                candidates.push(Reverse(ep));
                if include_deleted || !self.deleted[ep.id as usize] {
                    results.push(ep);
                }
            }
        }
        while results.len() > ef {
            results.pop();
        }

        while let Some(Reverse(current)) = candidates.pop() {
            if results.len() >= ef && results.peek().is_some_and(|w| current.dist > w.dist) {
                break;
            }
            let links = self.links(current.id, layer);
            for (j, slot) in links.iter().enumerate() {
                let neighbor = slot.load(Relaxed);
                if let Some(next) = links.get(j + 1) {
                    self.prefetch(next.load(Relaxed));
                }
                if self.levels[neighbor as usize] == NO_LEVEL || !visited.insert(neighbor) {
                    continue;
                }
                let dist = self.dist_to(query, neighbor);
                let full = results.len() >= ef;
                if full && results.peek().is_some_and(|w| dist >= w.dist) {
                    continue;
                }
                let scored = Scored { dist, id: neighbor };
                candidates.push(Reverse(scored));
                if include_deleted || !self.deleted[neighbor as usize] {
                    results.push(scored);
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }
        results.into_sorted_vec()
    }

    /// Chooses up to `m` neighbours from `candidates` (sorted closest first)
    /// with the diversity heuristic (Algorithm 4): a candidate is kept only if
    /// it is closer to the base than to every neighbour kept so far, so links
    /// point in different directions and reach other clusters. Remaining
    /// slots are filled with the closest discarded candidates
    /// (`keepPrunedConnections`), so nodes keep their full degree.
    fn select_neighbors(&self, candidates: &[Scored], m: usize) -> Vec<u32> {
        let mut kept: Vec<u32> = Vec::with_capacity(m);
        let mut discarded: Vec<u32> = Vec::new();
        for &c in candidates {
            if kept.len() >= m {
                break;
            }
            if kept.contains(&c.id) || discarded.contains(&c.id) {
                continue;
            }
            let diverse = kept.iter().all(|&k| c.dist < self.dist_between(c.id, k));
            if diverse {
                kept.push(c.id);
            } else {
                discarded.push(c.id);
            }
        }
        for id in discarded {
            if kept.len() >= m {
                break;
            }
            kept.push(id);
        }
        kept
    }

    /// Internal distance from a query to a stored node. Euclidean stays
    /// squared here (same order, no square root); see [`Self::reported`].
    #[inline]
    fn dist_to(&self, query: &Query<'_>, node: u32) -> f32 {
        let v = self.vectors.slot(node as usize);
        match self.metric {
            DistanceMetricKind::Cosine => cosine_from_dot(
                dot(query.v, v),
                query.inv_norm,
                self.inv_norms[node as usize],
            ),
            DistanceMetricKind::Euclidean => l2_squared(query.v, v),
            DistanceMetricKind::DotProduct => -dot(query.v, v),
        }
    }

    /// Internal distance between two stored nodes.
    #[inline]
    fn dist_between(&self, a: u32, b: u32) -> f32 {
        let (va, vb) = (self.vectors.slot(a as usize), self.vectors.slot(b as usize));
        match self.metric {
            DistanceMetricKind::Cosine => cosine_from_dot(
                dot(va, vb),
                self.inv_norms[a as usize],
                self.inv_norms[b as usize],
            ),
            DistanceMetricKind::Euclidean => l2_squared(va, vb),
            DistanceMetricKind::DotProduct => -dot(va, vb),
        }
    }

    /// Converts an internal distance to the one callers see.
    #[inline]
    fn reported(&self, d: f32) -> f32 {
        match self.metric {
            DistanceMetricKind::Euclidean => d.sqrt(),
            _ => d,
        }
    }

    /// Hints the CPU to start loading `node`'s vector.
    #[inline]
    fn prefetch(&self, node: u32) {
        #[cfg(target_arch = "x86_64")]
        {
            let v = self.vectors.slot(node as usize);
            // SAFETY: prefetching is a hint and never faults; SSE is part of
            // the x86-64 baseline.
            unsafe {
                use std::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
                _mm_prefetch::<_MM_HINT_T0>(v.as_ptr() as *const i8);
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = node;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distance::{CosineDistance, DistanceMetric};
    use std::collections::HashSet;

    fn make_index(dims: usize) -> HnswIndex {
        HnswIndex::new(
            crate::params::HnswParams {
                m: 8,
                m0: 16,
                ef_construction: 50,
                ef_search: 20,
                max_level: 5,
            },
            dims,
            DistanceMetricKind::Cosine,
        )
    }

    fn random_vector(dims: usize, seed: f32) -> Vec<f32> {
        (0..dims).map(|i| (seed * (i as f32 + 1.0)).sin()).collect()
    }

    #[test]
    fn test_insert_single() {
        let mut index = make_index(4);
        index.insert(NodeId(0), vec![1.0, 0.0, 0.0, 0.0]).unwrap();

        assert_eq!(index.len(), 1);
        assert!(index.entry_point().is_some());
        assert_eq!(index.entry_point(), Some(NodeId(0)));
    }

    #[test]
    fn test_search_empty_index() {
        let index = make_index(4);
        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 5, None).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_insert_and_search() {
        let mut index = make_index(8);

        // Insert 100 vectors. Seeds start at 0.1: seed 0 would produce the
        // all-zero vector, which has no direction and ties with everything.
        for i in 0..100 {
            let vec = random_vector(8, (i + 1) as f32 * 0.1);
            index.insert(NodeId(i), vec).unwrap();
        }

        // Search for the first inserted vector itself
        let query = random_vector(8, 0.1);
        let results = index.search(&query, 5, None).unwrap();

        assert!(!results.is_empty());
        assert!(results.len() <= 5);
        // The first result should be close to the query (same seed)
        assert_eq!(results[0].0, NodeId(0));
    }

    #[test]
    fn test_dimension_mismatch_on_insert() {
        let mut index = make_index(4);
        let result = index.insert(NodeId(0), vec![1.0, 2.0]);
        assert!(matches!(
            result,
            Err(AtlantisError::DimensionMismatch { .. })
        ));
    }

    #[test]
    fn test_dimension_mismatch_on_search() {
        let index = make_index(4);
        index.search(&[1.0, 2.0], 5, None).unwrap_err();
    }

    #[test]
    fn test_remove() {
        let mut index = make_index(4);
        index.insert(NodeId(0), vec![1.0, 0.0, 0.0, 0.0]).unwrap();
        index.insert(NodeId(1), vec![0.0, 1.0, 0.0, 0.0]).unwrap();

        assert_eq!(index.len(), 2);
        index.remove(NodeId(0));
        assert_eq!(index.len(), 1);

        // Removed node should not appear in search results
        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 5, None).unwrap();
        assert!(!results.iter().any(|(id, _)| *id == NodeId(0)));
    }

    #[test]
    fn test_recall() {
        // Test that HNSW recall is reasonable (at least 80% for this small dataset)
        let mut index = HnswIndex::new(
            crate::params::HnswParams {
                m: 16,
                m0: 32,
                ef_construction: 200,
                ef_search: 100,
                max_level: 16,
            },
            32,
            DistanceMetricKind::Cosine,
        );

        // Insert 500 random vectors
        let vectors: Vec<Vec<f32>> = (0..500)
            .map(|i| random_vector(32, i as f32 * 0.01))
            .collect();
        for (i, vec) in vectors.iter().enumerate() {
            index.insert(NodeId(i as u64), vec.clone()).unwrap();
        }

        // For each query, compare HNSW results with brute-force
        let mut total_recall = 0.0;
        let num_queries = 20;

        for q_idx in 0..num_queries {
            let query = &vectors[q_idx * 10];

            // Brute force top-5
            let mut brute: Vec<(NodeId, f32)> = vectors
                .iter()
                .enumerate()
                .map(|(i, v)| (NodeId(i as u64), CosineDistance.distance(query, v)))
                .collect();
            brute.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            let brute_top5: HashSet<NodeId> = brute.iter().take(5).map(|(id, _)| *id).collect();

            // HNSW top-5
            let hnsw_results = index.search(query, 5, None).unwrap();
            let hnsw_top5: HashSet<NodeId> = hnsw_results.iter().map(|(id, _)| *id).collect();

            let overlap = hnsw_top5.intersection(&brute_top5).count();
            total_recall += overlap as f32 / 5.0;
        }

        let avg_recall = total_recall / num_queries as f32;
        assert!(
            avg_recall > 0.80,
            "recall too low: {avg_recall:.2} (expected > 0.80)"
        );
    }

    #[test]
    fn test_search_with_params() {
        let mut index = make_index(4);

        for i in 0..10 {
            let vec = vec![i as f32, 0.0, 0.0, 0.0];
            index.insert(NodeId(i), vec).unwrap();
        }

        let params = SearchParams::top_k(3).with_threshold(0.5);
        let results = index
            .search_with_params(&[1.0, 0.0, 0.0, 0.0], &params)
            .unwrap();
        // With threshold 0.5, only very similar results are kept
        // cosine similarity of [1,0,0,0] and [1,0,0,0] = 1.0, distance = 0.0
        // threshold 0.5 means 1 - dist >= 0.5, so dist <= 0.5
        assert!(!results.is_empty());
    }

    #[test]
    fn test_reinsert_same_id_is_rejected() {
        let mut index = make_index(4);
        index.insert(NodeId(0), vec![1.0, 0.0, 0.0, 0.0]).unwrap();
        let err = index
            .insert(NodeId(0), vec![0.0, 1.0, 0.0, 0.0])
            .unwrap_err();
        assert!(matches!(err, AtlantisError::InvalidValue(_)));
        assert_eq!(
            index.get_vector(NodeId(0)),
            Some([1.0, 0.0, 0.0, 0.0].as_ref())
        );
    }

    #[test]
    fn test_links_are_capped() {
        let mut index = make_index(8);
        for i in 0..300 {
            index
                .insert(NodeId(i), random_vector(8, (i + 1) as f32 * 0.07))
                .unwrap();
        }
        for node in 0..300u32 {
            for l in 0..=index.levels[node as usize] as usize {
                let cap = if l == 0 {
                    index.params.m0
                } else {
                    index.params.m
                };
                let links = index.link_ids(node, l);
                assert!(
                    links.len() <= cap,
                    "node {node} has {} links at layer {l}",
                    links.len()
                );
                assert!(!links.contains(&node), "node {node} links to itself");
            }
        }
    }

    #[test]
    fn test_deleted_entry_point_still_leads_to_results() {
        // Whatever level each node draws, deleting the entry point must not
        // make the remaining nodes unreachable.
        for _ in 0..200 {
            let mut index = make_index(3);
            index.insert(NodeId(0), vec![1.0, 0.0, 0.0]).unwrap();
            index.insert(NodeId(1), vec![0.0, 1.0, 0.0]).unwrap();
            index.insert(NodeId(2), vec![0.7, 0.7, 0.0]).unwrap();
            let entry = index.entry_point().unwrap();
            index.remove(entry);
            let results = index.search(&[1.0, 0.1, 0.0], 1, None).unwrap();
            assert_eq!(results.len(), 1, "entry {entry:?} deleted, nothing found");
            assert_ne!(results[0].0, entry);
        }
    }

    #[test]
    fn test_get_vector() {
        let mut index = make_index(3);
        index.insert(NodeId(5), vec![1.0, 2.0, 3.0]).unwrap();

        assert_eq!(index.get_vector(NodeId(5)), Some([1.0, 2.0, 3.0].as_ref()));
        assert_eq!(index.get_vector(NodeId(99)), None);

        index.remove(NodeId(5));
        assert_eq!(index.get_vector(NodeId(5)), None);
    }
}

#[cfg(test)]
mod metric_tests {
    use super::*;
    use crate::distance::{DistanceMetric, EuclideanDistance};

    fn params() -> crate::params::HnswParams {
        crate::params::HnswParams {
            m: 8,
            m0: 16,
            ef_construction: 64,
            ef_search: 64,
            max_level: 5,
        }
    }

    #[test]
    fn euclidean_reports_true_distances() {
        let mut index = HnswIndex::new(params(), 2, DistanceMetricKind::Euclidean);
        for i in 0..50u64 {
            index.insert(NodeId(i), vec![i as f32, 0.0]).unwrap();
        }
        let hits = index.search(&[3.0, 4.0], 3, None).unwrap();
        assert_eq!(hits[0].0, NodeId(3));
        assert!((hits[0].1 - 4.0).abs() < 1e-5, "got {}", hits[0].1);
        assert!((hits[0].1 - EuclideanDistance.distance(&[3.0, 4.0], &[3.0, 0.0])).abs() < 1e-5);
    }

    #[test]
    fn dot_product_prefers_larger_projection() {
        let mut index = HnswIndex::new(params(), 2, DistanceMetricKind::DotProduct);
        for i in 1..20u64 {
            index.insert(NodeId(i), vec![i as f32, 1.0]).unwrap();
        }
        let hits = index.search(&[1.0, 0.0], 1, Some(64)).unwrap();
        assert_eq!(hits[0].0, NodeId(19));
        assert!((hits[0].1 + 19.0).abs() < 1e-5);
    }

    #[test]
    fn rejected_batch_inserts_nothing() {
        let mut index = HnswIndex::new(params(), 2, DistanceMetricKind::Cosine);
        index.insert(NodeId(0), vec![1.0, 0.0]).unwrap();
        // Duplicate inside the batch.
        let dup = vec![(NodeId(1), vec![0.0, 1.0]), (NodeId(1), vec![1.0, 1.0])];
        index.insert_batch(dup, 4).unwrap_err();
        // Id already present.
        let present = vec![(NodeId(2), vec![0.0, 1.0]), (NodeId(0), vec![1.0, 1.0])];
        index.insert_batch(present, 4).unwrap_err();
        // Wrong dimension in the last item.
        let short = vec![(NodeId(3), vec![0.0, 1.0]), (NodeId(4), vec![1.0])];
        index.insert_batch(short, 4).unwrap_err();
        assert_eq!(index.len(), 1);
        assert!(index.get_vector(NodeId(1)).is_none());
        assert!(index.get_vector(NodeId(3)).is_none());
    }

    #[test]
    fn small_batch_into_empty_index() {
        let mut index = HnswIndex::new(params(), 2, DistanceMetricKind::Euclidean);
        index.insert_batch(Vec::new(), 4).unwrap();
        assert!(index.entry_point().is_none());
        let items = (0..10u64)
            .map(|i| (NodeId(i), vec![i as f32, 0.0]))
            .collect();
        index.insert_batch(items, 4).unwrap();
        assert_eq!(index.len(), 10);
        assert_eq!(index.search(&[7.2, 0.0], 1, None).unwrap()[0].0, NodeId(7));
    }

    #[test]
    fn rejects_ids_beyond_u32() {
        let mut index = HnswIndex::new(params(), 2, DistanceMetricKind::Cosine);
        index
            .insert(NodeId(u32::MAX as u64), vec![1.0, 0.0])
            .unwrap_err();
    }

    #[test]
    fn sparse_ids_and_concurrent_searches() {
        let mut index = HnswIndex::new(params(), 4, DistanceMetricKind::Cosine);
        for i in 0..200u64 {
            let v: Vec<f32> = (0..4).map(|d| ((i * 4 + d) as f32 * 0.37).sin()).collect();
            index.insert(NodeId(i * 3), v).unwrap();
        }
        assert_eq!(index.len(), 200);
        let index = std::sync::Arc::new(index);
        let handles: Vec<_> = (0..4)
            .map(|t| {
                let index = index.clone();
                std::thread::spawn(move || {
                    for i in 0..50u64 {
                        let id = (t * 50 + i) * 3;
                        let v = index.get_vector(NodeId(id)).unwrap().to_vec();
                        let hits = index.search(&v, 1, Some(64)).unwrap();
                        assert_eq!(hits[0].0, NodeId(id));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
}
