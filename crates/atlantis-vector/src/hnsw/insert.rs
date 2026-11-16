//! Insertion: validating, preparing and linking nodes, one by one or in
//! parallel batches.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};

use rand::Rng;

use atlantis_core::{AtlantisError, NodeId};

use crate::distance::inverse_norm;

use super::scored::{Query, Scored};
use super::{lock, pack_entry, HnswIndex, NO_LEVEL};

/// Nodes a batch links on one thread before going parallel: threads that
/// all insert into a tiny graph contend on the same lists and pick poorer
/// neighbours.
const SEQUENTIAL_PREFIX: usize = 1024;

impl HnswIndex {
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
}
