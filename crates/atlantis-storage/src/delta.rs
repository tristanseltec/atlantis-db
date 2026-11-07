//! Delta log for incremental edge insertions.
//!
//! When edges are inserted, they go into the delta log first. CSR compaction
//! merges the delta into the main CSR structure. This avoids rebuilding the
//! CSR on every insert while still supporting incremental updates.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use atlantis_core::{EdgeId, NodeId};

/// A log of recent edge insertions that have not yet been compacted into CSR.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeltaLog {
    /// Maps source node ID to a list of `(target_node_id, edge_id)` pairs.
    entries: HashMap<NodeId, Vec<(NodeId, EdgeId)>>,
}

impl DeltaLog {
    /// Creates a new empty delta log.
    pub fn new() -> Self {
        DeltaLog::default()
    }

    /// Records a new edge insertion.
    pub fn insert(&mut self, from: NodeId, to: NodeId, edge_id: EdgeId) {
        self.entries.entry(from).or_default().push((to, edge_id));
    }

    /// Returns the edges for a given source node, or an empty slice.
    pub fn get(&self, from: NodeId) -> &[(NodeId, EdgeId)] {
        match self.entries.get(&from) {
            Some(v) => v.as_slice(),
            None => &[],
        }
    }

    /// Returns `true` if the delta log is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the total number of delta edges.
    pub fn len(&self) -> usize {
        self.entries.values().map(|v| v.len()).sum()
    }

    /// Drains all entries from the delta log, returning them as a flat list
    /// of `(from, to, edge_id)` tuples for CSR compaction.
    pub fn drain(&mut self) -> Vec<(NodeId, NodeId, EdgeId)> {
        let mut result = Vec::with_capacity(self.len());
        for (from, pairs) in self.entries.drain() {
            for (to, edge_id) in pairs {
                result.push((from, to, edge_id));
            }
        }
        result
    }

    /// Clears all entries.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Removes all delta entries for a given source node.
    pub fn remove_node(&mut self, from: NodeId) {
        self.entries.remove(&from);
    }

    /// Removes a specific edge from the delta log.
    pub fn remove_edge(&mut self, edge_id: EdgeId) {
        for pairs in self.entries.values_mut() {
            pairs.retain(|(_, eid)| *eid != edge_id);
        }
        // Clean up empty entries
        self.entries.retain(|_, v| !v.is_empty());
    }

    /// Returns an iterator over `(from, to, edge_id)` tuples.
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, NodeId, EdgeId)> + '_ {
        self.entries.iter().flat_map(|(&from, pairs)| {
            pairs.iter().map(move |&(to, edge_id)| (from, to, edge_id))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_get() {
        let mut delta = DeltaLog::new();
        delta.insert(NodeId(0), NodeId(1), EdgeId(0));
        delta.insert(NodeId(0), NodeId(2), EdgeId(1));
        delta.insert(NodeId(1), NodeId(2), EdgeId(2));

        assert_eq!(delta.get(NodeId(0)), [(NodeId(1), EdgeId(0)), (NodeId(2), EdgeId(1))]);
        assert_eq!(delta.get(NodeId(1)), [(NodeId(2), EdgeId(2))]);
        assert!(delta.get(NodeId(99)).is_empty());
    }

    #[test]
    fn test_len_and_empty() {
        let mut delta = DeltaLog::new();
        assert!(delta.is_empty());
        assert_eq!(delta.len(), 0);

        delta.insert(NodeId(0), NodeId(1), EdgeId(0));
        delta.insert(NodeId(0), NodeId(2), EdgeId(1));
        assert!(!delta.is_empty());
        assert_eq!(delta.len(), 2);
    }

    #[test]
    fn test_drain() {
        let mut delta = DeltaLog::new();
        delta.insert(NodeId(0), NodeId(1), EdgeId(0));
        delta.insert(NodeId(1), NodeId(2), EdgeId(1));

        let drained = delta.drain();
        assert_eq!(drained.len(), 2);
        assert!(drained.contains(&(NodeId(0), NodeId(1), EdgeId(0))));
        assert!(drained.contains(&(NodeId(1), NodeId(2), EdgeId(1))));

        // Delta should be empty after drain
        assert!(delta.is_empty());
    }

    #[test]
    fn test_clear() {
        let mut delta = DeltaLog::new();
        delta.insert(NodeId(0), NodeId(1), EdgeId(0));
        delta.clear();
        assert!(delta.is_empty());
    }

    #[test]
    fn test_remove_node() {
        let mut delta = DeltaLog::new();
        delta.insert(NodeId(0), NodeId(1), EdgeId(0));
        delta.insert(NodeId(0), NodeId(2), EdgeId(1));
        delta.insert(NodeId(1), NodeId(3), EdgeId(2));

        delta.remove_node(NodeId(0));
        assert!(delta.get(NodeId(0)).is_empty());
        assert_eq!(delta.get(NodeId(1)), [(NodeId(3), EdgeId(2))]);
        assert_eq!(delta.len(), 1);
    }

    #[test]
    fn test_remove_edge() {
        let mut delta = DeltaLog::new();
        delta.insert(NodeId(0), NodeId(1), EdgeId(0));
        delta.insert(NodeId(0), NodeId(2), EdgeId(1));

        delta.remove_edge(EdgeId(0));
        assert_eq!(delta.get(NodeId(0)), [(NodeId(2), EdgeId(1))]);
        assert_eq!(delta.len(), 1);
    }

    #[test]
    fn test_iter() {
        let mut delta = DeltaLog::new();
        delta.insert(NodeId(0), NodeId(1), EdgeId(0));
        delta.insert(NodeId(1), NodeId(2), EdgeId(1));

        let collected: Vec<_> = delta.iter().collect();
        assert_eq!(collected.len(), 2);
    }

    #[test]
    fn test_serde_roundtrip() {
        let mut delta = DeltaLog::new();
        delta.insert(NodeId(0), NodeId(1), EdgeId(0));
        delta.insert(NodeId(2), NodeId(3), EdgeId(1));

        let serialized = bincode::serialize(&delta).unwrap();
        let deserialized: DeltaLog = bincode::deserialize(&serialized).unwrap();
        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized.get(NodeId(0)), [(NodeId(1), EdgeId(0))]);
    }
}
