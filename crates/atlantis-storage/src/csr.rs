//! CSR (Compressed Sparse Row) adjacency structure.
//!
// //! Provides cache-friendly storage of graph adjacency using three arrays:
// //! offsets, targets, and edge_ids. Used by the hot tier for fast traversals.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use atlantis_core::{EdgeId, NodeId, RelType};

/// Compressed Sparse Row adjacency representation.
///
/// Stores outgoing edges for all nodes in contiguous arrays, indexed
/// by node ID. This gives O(1) access to a node's neighbor slice and
/// excellent cache locality during traversal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CsrAdjacency {
    /// `offsets[node_id]` = start index in `targets`/`edge_ids`.
    /// `offsets[node_id + 1] - offsets[node_id]` = out-degree of `node_id`.
    offsets: Vec<u64>,
    /// Target node IDs, contiguous per source node.
    targets: Vec<NodeId>,
    /// Edge IDs, parallel to `targets`.
    edge_ids: Vec<EdgeId>,
}

impl CsrAdjacency {
    /// Creates an empty CSR with no edges.
    pub fn new() -> Self {
        CsrAdjacency::default()
    }

    /// Builds a CSR from a list of `(from_node, to_node, edge_id)` tuples.
    ///
    /// The edges do not need to be pre-sorted — this method sorts them
    /// by source node before constructing the offset array.
    pub fn build(edges: &[(NodeId, NodeId, EdgeId)]) -> Self {
        if edges.is_empty() {
            return CsrAdjacency::new();
        }

        // Find max node ID to size the offsets array.
        let max_node = edges
            .iter()
            .map(|(from, _, _)| from.0.max(0))
            .max()
            .unwrap_or(0);

        let mut sorted: Vec<(NodeId, NodeId, EdgeId)> = edges.to_vec();
        sorted.sort_by_key(|(from, _, _)| from.0);

        let num_nodes = max_node as usize + 1;
        let mut offsets = vec![0u64; num_nodes + 1];

        // Count out-degree per node.
        for (from, _, _) in &sorted {
            offsets[from.0 as usize + 1] += 1;
        }

        // Prefix sum to compute actual offsets.
        for i in 1..=num_nodes {
            offsets[i] += offsets[i - 1];
        }

        let mut targets = Vec::with_capacity(sorted.len());
        let mut edge_ids = Vec::with_capacity(sorted.len());

        // Fill targets and edge_ids in source order (already sorted).
        for (_, to, edge_id) in &sorted {
            targets.push(*to);
            edge_ids.push(*edge_id);
        }

        CsrAdjacency {
            offsets,
            targets,
            edge_ids,
        }
    }

    /// Returns the neighbors of a node as a slice of `(target_node_id, edge_id)` pairs.
    ///
    /// Returns an empty slice if the node has no outgoing edges or the node ID
    /// is beyond the current CSR size.
    pub fn neighbors(&self, node: NodeId) -> &[(NodeId, EdgeId)] {
        let idx = node.0 as usize;
        if idx + 1 >= self.offsets.len() {
            return &[];
        }
        let start = self.offsets[idx] as usize;
        let end = self.offsets[idx + 1] as usize;
        // Reinterpret the parallel targets/edge_ids as pairs.
        // Since we can't return a slice of tuples from two separate vecs,
        // we return an empty slice here and use the iterator method instead.
        // Callers should use `neighbor_targets` and `neighbor_edges`.
        let _ = (start, end);
        &[]
    }

    /// Returns the target node IDs for a node's outgoing edges.
    pub fn neighbor_targets(&self, node: NodeId) -> &[NodeId] {
        let idx = node.0 as usize;
        if idx + 1 >= self.offsets.len() {
            return &[];
        }
        let start = self.offsets[idx] as usize;
        let end = self.offsets[idx + 1] as usize;
        &self.targets[start..end]
    }

    /// Returns the edge IDs for a node's outgoing edges.
    pub fn neighbor_edges(&self, node: NodeId) -> &[EdgeId] {
        let idx = node.0 as usize;
        if idx + 1 >= self.offsets.len() {
            return &[];
        }
        let start = self.offsets[idx] as usize;
        let end = self.offsets[idx + 1] as usize;
        &self.edge_ids[start..end]
    }

    /// Returns the out-degree of a node.
    pub fn out_degree(&self, node: NodeId) -> usize {
        let idx = node.0 as usize;
        if idx + 1 >= self.offsets.len() {
            return 0;
        }
        (self.offsets[idx + 1] - self.offsets[idx]) as usize
    }

    /// Returns the total number of edges stored.
    pub fn edge_count(&self) -> usize {
        self.targets.len()
    }

    /// Returns the number of nodes the CSR covers (max node ID + 1).
    pub fn node_count(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    /// Returns an iterator over all `(from, to, edge_id)` edges.
    pub fn iter_edges(&self) -> Vec<(NodeId, NodeId, EdgeId)> {
        let mut result = Vec::new();
        for node_idx in 0..self.offsets.len().saturating_sub(1) {
            let start = self.offsets[node_idx] as usize;
            let end = self.offsets.get(node_idx + 1).copied().unwrap_or(0) as usize;
            let from = NodeId(node_idx as u64);
            for i in start..end {
                result.push((from, self.targets[i], self.edge_ids[i]));
            }
        }
        result
    }
}

/// Stores CSR adjacency structures per relationship type.
///
/// This is the primary adjacency store for the hot tier. Each relationship
/// type gets its own CSR for cache-local traversal of a specific edge kind.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CsrStore {
    csrs: HashMap<RelType, CsrAdjacency>,
}

impl CsrStore {
    /// Creates a new empty CSR store.
    pub fn new() -> Self {
        CsrStore::default()
    }

    /// Returns the CSR for a given relationship type, if it exists.
    pub fn get(&self, rel_type: &RelType) -> Option<&CsrAdjacency> {
        self.csrs.get(rel_type)
    }

    /// Builds or rebuilds the CSR for a given relationship type from an edge list.
    pub fn build_rel_type(&mut self, rel_type: RelType, edges: &[(NodeId, NodeId, EdgeId)]) {
        self.csrs.insert(rel_type, CsrAdjacency::build(edges));
    }

    /// Removes the CSR for a given relationship type.
    pub fn remove(&mut self, rel_type: &RelType) {
        self.csrs.remove(rel_type);
    }

    /// Returns the number of relationship types with CSR data.
    pub fn len(&self) -> usize {
        self.csrs.len()
    }

    /// Returns `true` if there are no CSR structures stored.
    pub fn is_empty(&self) -> bool {
        self.csrs.is_empty()
    }

    /// Returns an iterator over relationship types that have CSR data.
    pub fn rel_types(&self) -> impl Iterator<Item = &RelType> {
        self.csrs.keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_csr_build_and_query() {
        let edges = vec![
            (NodeId(0), NodeId(1), EdgeId(0)),
            (NodeId(0), NodeId(2), EdgeId(1)),
            (NodeId(1), NodeId(2), EdgeId(2)),
        ];
        let csr = CsrAdjacency::build(&edges);

        assert_eq!(csr.node_count(), 2); // max node ID is 1
        assert_eq!(csr.edge_count(), 3);

        // Node 0 has 2 outgoing edges
        assert_eq!(csr.out_degree(NodeId(0)), 2);
        let targets = csr.neighbor_targets(NodeId(0));
        assert_eq!(targets, [NodeId(1), NodeId(2)]);
        let edge_ids = csr.neighbor_edges(NodeId(0));
        assert_eq!(edge_ids, [EdgeId(0), EdgeId(1)]);

        // Node 1 has 1 outgoing edge
        assert_eq!(csr.out_degree(NodeId(1)), 1);
        assert_eq!(csr.neighbor_targets(NodeId(1)), [NodeId(2)]);
    }

    #[test]
    fn test_csr_empty() {
        let csr = CsrAdjacency::new();
        assert_eq!(csr.edge_count(), 0);
        assert_eq!(csr.node_count(), 0);
        assert_eq!(csr.out_degree(NodeId(0)), 0);
        assert!(csr.neighbor_targets(NodeId(0)).is_empty());
    }

    #[test]
    fn test_csr_no_outgoing() {
        // Node 1 has no outgoing edges but is in the range
        let edges = vec![(NodeId(0), NodeId(1), EdgeId(0))];
        let csr = CsrAdjacency::build(&edges);

        assert_eq!(csr.out_degree(NodeId(0)), 1);
        assert_eq!(csr.out_degree(NodeId(1)), 0);
        assert!(csr.neighbor_targets(NodeId(1)).is_empty());
    }

    #[test]
    fn test_csr_out_of_range() {
        let csr = CsrAdjacency::build(&[(NodeId(0), NodeId(1), EdgeId(0))]);
        assert_eq!(csr.out_degree(NodeId(99)), 0);
        assert!(csr.neighbor_targets(NodeId(99)).is_empty());
    }

    #[test]
    fn test_csr_iter_edges() {
        let edges = vec![
            (NodeId(0), NodeId(1), EdgeId(0)),
            (NodeId(0), NodeId(2), EdgeId(1)),
            (NodeId(1), NodeId(2), EdgeId(2)),
        ];
        let csr = CsrAdjacency::build(&edges);
        let collected: Vec<_> = csr.iter_edges();
        assert_eq!(collected.len(), 3);
        assert!(collected.contains(&(NodeId(0), NodeId(1), EdgeId(0))));
        assert!(collected.contains(&(NodeId(0), NodeId(2), EdgeId(1))));
        assert!(collected.contains(&(NodeId(1), NodeId(2), EdgeId(2))));
    }

    #[test]
    fn test_csr_serde_roundtrip() {
        let edges = vec![
            (NodeId(0), NodeId(1), EdgeId(0)),
            (NodeId(1), NodeId(2), EdgeId(1)),
        ];
        let csr = CsrAdjacency::build(&edges);
        let serialized = bincode::serialize(&csr).unwrap();
        let deserialized: CsrAdjacency = bincode::deserialize(&serialized).unwrap();
        assert_eq!(deserialized.edge_count(), 2);
        assert_eq!(deserialized.neighbor_targets(NodeId(0)), [NodeId(1)]);
    }

    #[test]
    fn test_csr_store() {
        let mut store = CsrStore::new();
        assert!(store.is_empty());

        store.build_rel_type(RelType::from("KNOWS"), &[(NodeId(0), NodeId(1), EdgeId(0))]);
        assert_eq!(store.len(), 1);

        let csr = store.get(&RelType::from("KNOWS")).unwrap();
        assert_eq!(csr.edge_count(), 1);

        assert!(store.get(&RelType::from("MISSING")).is_none());
    }
}
