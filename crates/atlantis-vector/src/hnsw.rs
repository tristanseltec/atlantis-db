//! HNSW (Hierarchical Navigable Small World) vector index.
//!
//! Implements the HNSW algorithm for approximate nearest neighbor search.
//! Builds a multi-layer graph where upper layers are sparse (for fast
//! navigation) and the base layer is dense (for accurate search).

use std::collections::{HashMap, HashSet};

use rand::Rng;

use atlantis_core::{AtlantisError, NodeId};

use crate::distance::DistanceMetric;
use crate::params::SearchParams;
use crate::vector_store::VectorStore;

/// A single layer of the HNSW graph.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Layer {
    /// Adjacency: node ID -> list of neighbor node IDs at this layer.
    adjacency: HashMap<NodeId, Vec<NodeId>>,
}

impl Layer {
    fn new() -> Self {
        Layer {
            adjacency: HashMap::new(),
        }
    }

    fn add_edge(&mut self, a: NodeId, b: NodeId) {
        self.adjacency.entry(a).or_default().push(b);
        self.adjacency.entry(b).or_default().push(a);
    }

    fn neighbors(&self, node: NodeId) -> &[NodeId] {
        self.adjacency
            .get(&node)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
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
    pub fn insert(&mut self, node_id: NodeId, vector: Vec<f32>) -> Result<(), AtlantisError> {
        // Validate dimensions
        if vector.len() != self.vectors.dimensions() {
            return Err(AtlantisError::DimensionMismatch {
                expected: self.vectors.dimensions(),
                actual: vector.len(),
            });
        }

        // Store the vector
        self.vectors.insert_at(node_id, vector)?;

        // Assign random level
        let level = self.random_level();

        // If this is the first insert, set as entry point
        if self.entry_point.is_none() {
            self.entry_point = Some(node_id);
            self.max_level = level;
            // Ensure the node exists in adjacency at its level
            for l in 0..=level {
                self.layers[l].adjacency.entry(node_id).or_default();
            }
            return Ok(());
        }

        let entry = self.entry_point.unwrap();

        // Greedy search from top down to level+1
        let mut current = entry;
        for l in (level + 1..=self.max_level).rev() {
            current = self.greedy_search_layer(current, l);
        }

        // Insert at each layer from level down to 0
        for l in (0..=level).rev() {
            let ef = self.params.ef_construction;
            let candidates = self.search_layer(node_id, current, ef, l);

            // Select M nearest neighbors
            let m = if l == 0 {
                self.params.m0
            } else {
                self.params.m
            };
            let neighbors = self.select_neighbors(&candidates, m, l);

            // Add bidirectional edges
            for &neighbor in &neighbors {
                self.layers[l].add_edge(node_id, neighbor);

                // Prune neighbor's connections if exceeded max
                let max_conn = if l == 0 {
                    self.params.m0
                } else {
                    self.params.m
                };
                self.prune_connections(neighbor, max_conn, l);
            }

            current = neighbors.first().copied().unwrap_or(current);
        }

        // Update entry point if new max level
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

        if self.entry_point.is_none() {
            return Ok(Vec::new());
        }

        let entry = self.entry_point.unwrap();
        let ef = ef_search.unwrap_or(self.params.ef_search).max(k);

        // Greedy search from top down to layer 1
        let mut current = entry;
        for l in (1..=self.max_level).rev() {
            current = self.greedy_search_layer_with_vector(query, current, l);
        }

        // Search at layer 0 with ef
        let candidates = self.search_layer_with_vector(query, current, ef, 0);

        // Filter out deleted nodes, sort by distance, take top-k
        let mut results: Vec<(NodeId, f32)> = candidates
            .into_iter()
            .filter(|(id, _)| !self.deleted.contains(id) && self.vectors.contains(*id))
            .collect();
        results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
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
    pub fn remove(&mut self, node_id: NodeId) {
        self.deleted.insert(node_id);
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
        let ml = 1.0 / (self.params.m as f64).ln();
        let uniform: f64 = rand::thread_rng().gen_range(f64::EPSILON..1.0);
        let level = (-uniform.ln() * ml).floor() as usize;
        level.min(self.params.max_level)
    }

    /// Greedy search at a single layer: find the closest node to `target_node`
    /// by following the closest neighbor at each step.
    fn greedy_search_layer(&self, start: NodeId, layer: usize) -> NodeId {
        if self.vectors.get(start).is_none() {
            return start;
        }
        let mut current = start;
        let mut current_dist = self.dist_to_node(start, start);

        loop {
            let neighbors = self.layers[layer].neighbors(current);
            let mut found_closer = false;

            for &neighbor in neighbors {
                if self.deleted.contains(&neighbor) || !self.vectors.contains(neighbor) {
                    continue;
                }
                let dist = self.dist_between_nodes(current, neighbor);
                if dist < current_dist {
                    current_dist = dist;
                    current = neighbor;
                    found_closer = true;
                }
            }

            if !found_closer {
                break;
            }
        }

        current
    }

    /// Greedy search at a single layer using a raw query vector.
    fn greedy_search_layer_with_vector(
        &self,
        query: &[f32],
        start: NodeId,
        layer: usize,
    ) -> NodeId {
        let mut current = start;
        let mut current_dist = self
            .distance
            .distance(query, self.vectors.get(start).unwrap_or(&[]));

        loop {
            let neighbors = self.layers[layer].neighbors(current);
            let mut found_closer = false;

            for &neighbor in neighbors {
                if self.deleted.contains(&neighbor) || !self.vectors.contains(neighbor) {
                    continue;
                }
                if let Some(vec) = self.vectors.get(neighbor) {
                    let dist = self.distance.distance(query, vec);
                    if dist < current_dist {
                        current_dist = dist;
                        current = neighbor;
                        found_closer = true;
                    }
                }
            }

            if !found_closer {
                break;
            }
        }

        current
    }

    /// Search a layer with a candidate pool of size `ef`.
    /// Returns candidates sorted by distance to the inserting node.
    fn search_layer(
        &self,
        target_node: NodeId,
        entry: NodeId,
        ef: usize,
        layer: usize,
    ) -> Vec<(NodeId, f32)> {
        let target_vec = self.vectors.get(target_node).unwrap_or(&[]).to_vec();
        self.search_layer_with_vector(&target_vec, entry, ef, layer)
    }

    /// Search a layer with a raw query vector.
    fn search_layer_with_vector(
        &self,
        query: &[f32],
        entry: NodeId,
        ef: usize,
        layer: usize,
    ) -> Vec<(NodeId, f32)> {
        let mut visited: HashSet<NodeId> = HashSet::new();
        let mut candidates: Vec<(NodeId, f32)> = Vec::new();
        let mut results: Vec<(NodeId, f32)> = Vec::new();

        let entry_dist = if let Some(vec) = self.vectors.get(entry) {
            self.distance.distance(query, vec)
        } else {
            return results;
        };

        visited.insert(entry);
        candidates.push((entry, entry_dist));
        results.push((entry, entry_dist));

        while let Some(&(closest_id, closest_dist)) = candidates.last() {
            // Find the farthest in results
            let farthest_dist = results.iter().map(|(_, d)| *d).fold(f32::MIN, f32::max);

            if closest_dist > farthest_dist && results.len() >= ef {
                break;
            }

            // Pop the closest candidate
            let (curr_id, _) = candidates.pop().unwrap_or((closest_id, closest_dist));
            let neighbors = self.layers[layer].neighbors(curr_id);

            for &neighbor in neighbors {
                if visited.contains(&neighbor) {
                    continue;
                }
                visited.insert(neighbor);

                if self.deleted.contains(&neighbor) || !self.vectors.contains(neighbor) {
                    continue;
                }

                if let Some(vec) = self.vectors.get(neighbor) {
                    let dist = self.distance.distance(query, vec);
                    let farthest = results.iter().map(|(_, d)| *d).fold(f32::MIN, f32::max);

                    if results.len() < ef || dist < farthest {
                        candidates.push((neighbor, dist));
                        results.push((neighbor, dist));

                        if results.len() > ef {
                            // Remove the farthest
                            results.sort_by(|a, b| {
                                a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
                            });
                            results.truncate(ef);
                        }
                    }
                }
            }

            // Sort candidates by distance (closest first = last in vec for pop)
            candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        }

        results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    /// Selects the M nearest neighbors from a candidate list.
    fn select_neighbors(
        &self,
        candidates: &[(NodeId, f32)],
        m: usize,
        _layer: usize,
    ) -> Vec<NodeId> {
        candidates.iter().take(m).map(|(id, _)| *id).collect()
    }

    /// Prunes a node's connections to stay within the max limit.
    fn prune_connections(&mut self, node: NodeId, max_conn: usize, layer: usize) {
        // Collect the current connections and compute the pruned set
        let (pruned, removed): (Vec<NodeId>, Vec<NodeId>) = {
            let connections = match self.layers[layer].adjacency.get(&node) {
                Some(c) => c.clone(),
                None => return,
            };

            if connections.len() <= max_conn {
                return;
            }

            // Sort by distance and keep the closest max_conn
            let mut scored: Vec<(NodeId, f32)> = connections
                .iter()
                .filter_map(|&neighbor| {
                    if self.deleted.contains(&neighbor) || !self.vectors.contains(neighbor) {
                        return None;
                    }
                    let dist = self.dist_between_nodes(node, neighbor);
                    Some((neighbor, dist))
                })
                .collect();
            scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

            let pruned: Vec<NodeId> = scored.iter().take(max_conn).map(|(id, _)| *id).collect();
            let removed: Vec<NodeId> = connections
                .iter()
                .filter(|n| !pruned.contains(n))
                .copied()
                .collect();

            (pruned, removed)
        };

        // Update the node's connections
        if let Some(conns) = self.layers[layer].adjacency.get_mut(&node) {
            *conns = pruned;
        }

        // Remove the node from the removed neighbors' lists
        for &r in &removed {
            if let Some(neighbor_conns) = self.layers[layer].adjacency.get_mut(&r) {
                neighbor_conns.retain(|n| *n != node);
            }
        }
    }

    /// Computes distance between two stored nodes.
    fn dist_between_nodes(&self, a: NodeId, b: NodeId) -> f32 {
        let va = self.vectors.get(a).unwrap_or(&[]);
        let vb = self.vectors.get(b).unwrap_or(&[]);
        self.distance.distance(va, vb)
    }

    /// Computes distance from a node to itself (used for initial distance).
    fn dist_to_node(&self, a: NodeId, b: NodeId) -> f32 {
        self.dist_between_nodes(a, b)
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
    fn test_get_vector() {
        let mut index = make_index(3);
        index.insert(NodeId(5), vec![1.0, 2.0, 3.0]).unwrap();

        assert_eq!(index.get_vector(NodeId(5)), Some([1.0, 2.0, 3.0].as_ref()));
        assert_eq!(index.get_vector(NodeId(99)), None);

        index.remove(NodeId(5));
        assert_eq!(index.get_vector(NodeId(5)), None);
    }
}
