//! HNSW (Hierarchical Navigable Small World) vector index.
//!
//! Implements the HNSW algorithm for approximate nearest neighbor search.
//! Builds a multi-layer graph where upper layers are sparse (for fast
//! navigation) and the base layer is dense (for accurate search).

use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap, HashSet};

use rand::Rng;

use atlantis_core::{AtlantisError, NodeId};

use crate::distance::DistanceMetric;
use crate::params::SearchParams;
use crate::vector_store::VectorStore;

/// A single layer of the HNSW graph.
///
/// Edges are **directed**: `adjacency[a]` is the list of nodes `a` links to.
/// When a list is pruned, only that list changes; the nodes that link *to*
/// `a` keep their links. Storing edges undirected and pruning both
/// directions (the previous design) silently disconnected nodes.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Layer {
    /// Adjacency: node ID -> the node IDs it links to at this layer.
    adjacency: HashMap<NodeId, Vec<NodeId>>,
}

impl Layer {
    fn new() -> Self {
        Layer {
            adjacency: HashMap::new(),
        }
    }

    fn neighbors(&self, node: NodeId) -> &[NodeId] {
        self.adjacency
            .get(&node)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

/// A node and its distance to the current query, ordered by distance
/// (then id, so equal distances order deterministically).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Scored {
    dist: f32,
    id: NodeId,
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
            .then(self.id.0.cmp(&other.id.0))
    }
}

/// The HNSW vector index.
pub struct HnswIndex {
    params: crate::params::HnswParams,
    layers: Vec<Layer>,
    vectors: VectorStore,
    entry_point: Option<NodeId>,
    max_level: usize,
    distance: Box<dyn DistanceMetric>,
    deleted: HashSet<NodeId>,
}

impl HnswIndex {
    /// Creates a new HNSW index with the given parameters and distance metric.
    pub fn new(
        params: crate::params::HnswParams,
        dimensions: usize,
        distance: Box<dyn DistanceMetric>,
    ) -> Self {
        let max_level = params.max_level;
        HnswIndex {
            params,
            layers: vec![Layer::new(); max_level + 1],
            vectors: VectorStore::with_capacity(dimensions, 1024),
            entry_point: None,
            max_level: 0,
            distance,
            deleted: HashSet::new(),
        }
    }

    /// Returns the index parameters.
    pub fn params(&self) -> &crate::params::HnswParams {
        &self.params
    }

    /// Returns the number of active (non-deleted) vectors.
    pub fn len(&self) -> usize {
        self.vectors.len() - self.deleted.len()
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
        self.entry_point
    }

    /// Returns the current max level of the index.
    pub fn max_level(&self) -> usize {
        self.max_level
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
        if vector.len() != self.vectors.dimensions() {
            return Err(AtlantisError::DimensionMismatch {
                expected: self.vectors.dimensions(),
                actual: vector.len(),
            });
        }
        if self.vectors.contains(node_id) {
            // Links were chosen for the old vector, and other nodes link to
            // this id; replacing it in place would leave a wrong graph.
            return Err(AtlantisError::InvalidValue(format!(
                "HNSW node {} already exists; insert the new vector under a new id",
                node_id.0
            )));
        }
        let query = vector.clone();
        self.vectors.insert_at(node_id, vector)?;
        let level = self.random_level();

        let Some(entry) = self.entry_point else {
            self.entry_point = Some(node_id);
            self.max_level = level;
            for layer in &mut self.layers[..=level] {
                layer.adjacency.entry(node_id).or_default();
            }
            return Ok(());
        };

        let mut entry_points = vec![Scored {
            dist: self.dist_to(&query, entry),
            id: entry,
        }];
        for l in (level + 1..=self.max_level).rev() {
            entry_points = self.search_layer(&query, &entry_points, 1, l, true);
        }

        for l in (0..=level.min(self.max_level)).rev() {
            let found =
                self.search_layer(&query, &entry_points, self.params.ef_construction, l, true);
            let candidates: Vec<Scored> = found
                .iter()
                .copied()
                .filter(|c| c.id != node_id && !self.deleted.contains(&c.id))
                .collect();
            let neighbors = self.select_neighbors(&candidates, self.params.m);
            self.layers[l].adjacency.insert(node_id, neighbors.clone());

            let cap = if l == 0 {
                self.params.m0
            } else {
                self.params.m
            };
            for neighbor in neighbors {
                let list = self.layers[l].adjacency.entry(neighbor).or_default();
                list.push(node_id);
                if list.len() > cap {
                    self.shrink(neighbor, cap, l);
                }
            }
            if !found.is_empty() {
                entry_points = found;
            }
        }
        // Layers above the current top exist only for this node.
        for layer in &mut self.layers[self.max_level + 1..=level.max(self.max_level)] {
            layer.adjacency.entry(node_id).or_default();
        }
        if level > self.max_level {
            self.max_level = level;
            self.entry_point = Some(node_id);
        }
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
        let Some(entry) = self.entry_point else {
            return Ok(Vec::new());
        };
        let ef = ef_search.unwrap_or(self.params.ef_search).max(k);

        let mut entry_points = vec![Scored {
            dist: self.dist_to(query, entry),
            id: entry,
        }];
        for l in (1..=self.max_level).rev() {
            entry_points = self.search_layer(query, &entry_points, 1, l, true);
        }
        let mut results: Vec<(NodeId, f32)> = self
            .search_layer(query, &entry_points, ef, 0, false)
            .into_iter()
            .map(|c| (c.id, c.dist))
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
            self.deleted.insert(node_id);
        }
    }

    /// Returns the vector for a given node ID, if it exists.
    pub fn get_vector(&self, node_id: NodeId) -> Option<&[f32]> {
        if self.deleted.contains(&node_id) {
            return None;
        }
        self.vectors.get(node_id)
    }

    /// Returns the number of layers in the index.
    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    // ---- Internal methods ----

    /// Assigns a random level using the HNSW formula:
    /// `level = floor(-ln(unif(0,1)) * mL)` where `mL = 1 / ln(M)`.
    fn random_level(&self) -> usize {
        let ml = 1.0 / (self.params.m.max(2) as f64).ln();
        let uniform: f64 = rand::thread_rng().gen_range(f64::EPSILON..1.0);
        let level = (-uniform.ln() * ml).floor() as usize;
        level.min(self.params.max_level)
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
        query: &[f32],
        entry_points: &[Scored],
        ef: usize,
        layer: usize,
        include_deleted: bool,
    ) -> Vec<Scored> {
        let ef = ef.max(1);
        let mut visited: HashSet<NodeId> = HashSet::with_capacity(ef * 8);
        // Min-heap of nodes to expand; max-heap of the best `ef` found.
        let mut candidates: BinaryHeap<Reverse<Scored>> = BinaryHeap::new();
        let mut results: BinaryHeap<Scored> = BinaryHeap::new();

        for &ep in entry_points {
            if visited.insert(ep.id) {
                candidates.push(Reverse(ep));
                if include_deleted || !self.deleted.contains(&ep.id) {
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
            for &neighbor in self.layers[layer].neighbors(current.id) {
                if !visited.insert(neighbor) {
                    continue;
                }
                let Some(vec) = self.vectors.get(neighbor).filter(|v| !v.is_empty()) else {
                    continue;
                };
                let dist = self.distance.distance(query, vec);
                let full = results.len() >= ef;
                if full && results.peek().is_some_and(|w| dist >= w.dist) {
                    continue;
                }
                let scored = Scored { dist, id: neighbor };
                candidates.push(Reverse(scored));
                if include_deleted || !self.deleted.contains(&neighbor) {
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
    fn select_neighbors(&self, candidates: &[Scored], m: usize) -> Vec<NodeId> {
        let mut kept: Vec<Scored> = Vec::with_capacity(m);
        let mut discarded: Vec<NodeId> = Vec::new();
        for &c in candidates {
            if kept.len() >= m {
                break;
            }
            let diverse = kept
                .iter()
                .all(|k| c.dist < self.dist_between_nodes(c.id, k.id));
            if diverse {
                kept.push(c);
            } else {
                discarded.push(c.id);
            }
        }
        let mut out: Vec<NodeId> = kept.into_iter().map(|k| k.id).collect();
        for id in discarded {
            if out.len() >= m {
                break;
            }
            out.push(id);
        }
        out
    }

    /// Shrinks `node`'s link list at `layer` to `cap` with the heuristic.
    /// Only `node`'s own list changes.
    fn shrink(&mut self, node: NodeId, cap: usize, layer: usize) {
        let Some(base) = self.vectors.get(node) else {
            return;
        };
        let mut scored: Vec<Scored> = self.layers[layer]
            .neighbors(node)
            .iter()
            .filter(|n| !self.deleted.contains(n))
            .filter_map(|&n| {
                self.vectors.get(n).map(|v| Scored {
                    dist: self.distance.distance(base, v),
                    id: n,
                })
            })
            .collect();
        scored.sort_unstable();
        scored.dedup_by_key(|s| s.id);
        let kept = self.select_neighbors(&scored, cap);
        self.layers[layer].adjacency.insert(node, kept);
    }

    /// Distance from a query vector to a stored node (`f32::MAX` if absent).
    fn dist_to(&self, query: &[f32], node: NodeId) -> f32 {
        match self.vectors.get(node) {
            Some(v) if !v.is_empty() => self.distance.distance(query, v),
            _ => f32::MAX,
        }
    }

    /// Computes distance between two stored nodes.
    fn dist_between_nodes(&self, a: NodeId, b: NodeId) -> f32 {
        match (self.vectors.get(a), self.vectors.get(b)) {
            (Some(va), Some(vb)) if !va.is_empty() && !vb.is_empty() => {
                self.distance.distance(va, vb)
            }
            _ => f32::MAX,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::distance::CosineDistance;

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
            Box::new(CosineDistance),
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
            Box::new(CosineDistance),
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
                .map(|(i, v)| (NodeId(i as u64), index.distance.distance(query, v)))
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
        for (l, layer) in index.layers.iter().enumerate() {
            let cap = if l == 0 {
                index.params.m0
            } else {
                index.params.m
            };
            for (node, links) in &layer.adjacency {
                assert!(
                    links.len() <= cap,
                    "node {node:?} has {} links at layer {l}",
                    links.len()
                );
                assert!(!links.contains(node), "node {node:?} links to itself");
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
