//! In-memory hot tier storage with CSR + delta adjacency.
//!
//! Stores frequently accessed nodes, relationships, and adjacency data in
//! memory for low-latency access. Uses CSR for read-optimized traversal
//! and a delta log for incremental writes.

use std::collections::HashMap;

use atlantis_core::{
    Direction, EdgeId, Label, LabelSet, Node, NodeId, PropertyMap, RelType, Relationship, Value,
};

use crate::csr::CsrStore;
use crate::delta::DeltaLog;
use crate::lru::LruTracker;

/// In-memory storage tier holding hot (frequently accessed) data.
///
/// Combines CSR adjacency with a delta log for incremental updates.
/// The delta is periodically compacted into CSR by a background actor.
pub struct HotTier {
    /// Node storage — indexed by NodeId for O(1) direct access.
    nodes: Vec<Option<Node>>,
    /// Free slots in the nodes vector for reuse after deletion.
    free_node_slots: Vec<usize>,
    /// Edge storage — indexed by EdgeId.
    edges: Vec<Option<Relationship>>,
    /// Free slots in the edges vector for reuse after deletion.
    free_edge_slots: Vec<usize>,
    /// CSR adjacency per relationship type.
    csr: CsrStore,
    /// Delta logs per relationship type for incremental inserts.
    deltas: HashMap<RelType, DeltaLog>,
    /// Label index: maps label to all node IDs with that label.
    label_index: HashMap<Label, Vec<NodeId>>,
    /// LRU tracker for tiering decisions.
    access_tracker: LruTracker,
    /// Delta size threshold per rel_type before triggering compaction.
    compaction_threshold: usize,
    /// Next node ID to allocate.
    next_node_id: u64,
    /// Next edge ID to allocate.
    next_edge_id: u64,
    /// Node IDs that have been deleted (ghost set for tombstone tracking).
    deleted_nodes: std::collections::HashSet<NodeId>,
}

impl HotTier {
    /// Creates a new empty hot tier with the given compaction threshold.
    pub fn new(compaction_threshold: usize) -> Self {
        HotTier {
            nodes: Vec::new(),
            free_node_slots: Vec::new(),
            edges: Vec::new(),
            free_edge_slots: Vec::new(),
            csr: CsrStore::new(),
            deltas: HashMap::new(),
            label_index: HashMap::new(),
            access_tracker: LruTracker::new(),
            compaction_threshold,
            next_node_id: 0,
            next_edge_id: 0,
            deleted_nodes: std::collections::HashSet::new(),
        }
    }

    /// Returns the number of active nodes.
    pub fn node_count(&self) -> u64 {
        self.next_node_id - self.deleted_nodes.len() as u64
    }

    /// Returns the number of active edges.
    pub fn edge_count(&self) -> u64 {
        self.next_edge_id
    }

    /// Inserts a new node, returning its ID.
    pub fn insert_node(&mut self, labels: LabelSet, properties: PropertyMap) -> NodeId {
        let id = NodeId(self.next_node_id);
        self.next_node_id += 1;

        let node = Node {
            id,
            labels,
            properties,
        };

        // Update label index
        for label in node.labels.iter() {
            self.label_index.entry(label.clone()).or_default().push(id);
        }

        let slot = if let Some(slot) = self.free_node_slots.pop() {
            self.nodes[slot] = Some(node);
            slot
        } else {
            self.nodes.push(Some(node));
            self.nodes.len() - 1
        };

        self.access_tracker.touch(id);
        let _ = slot; // slot tracked for future reuse support
        id
    }

    /// Returns a reference to the node with the given ID, if it exists and is not deleted.
    pub fn get_node(&self, id: NodeId) -> Option<&Node> {
        if self.deleted_nodes.contains(&id) {
            return None;
        }
        self.nodes.get(id.0 as usize).and_then(|n| n.as_ref())
    }

    /// Returns a mutable reference to the node with the given ID.
    pub fn get_node_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        if self.deleted_nodes.contains(&id) {
            return None;
        }
        self.nodes.get_mut(id.0 as usize).and_then(|n| n.as_mut())
    }

    /// Inserts a new relationship (directed edge), returning its ID.
    pub fn insert_edge(
        &mut self,
        from: NodeId,
        to: NodeId,
        rel_type: RelType,
        properties: PropertyMap,
    ) -> EdgeId {
        let id = EdgeId(self.next_edge_id);
        self.next_edge_id += 1;

        let edge = Relationship {
            id,
            from,
            to,
            rel_type: rel_type.clone(),
            properties,
        };

        let slot = if let Some(slot) = self.free_edge_slots.pop() {
            self.edges[slot] = Some(edge);
            slot
        } else {
            self.edges.push(Some(edge));
            self.edges.len() - 1
        };

        // Add to delta log
        self.deltas
            .entry(rel_type)
            .or_default()
            .insert(from, to, id);

        self.access_tracker.touch(from);
        let _ = slot;
        id
    }

    /// Returns a reference to the edge with the given ID.
    pub fn get_edge(&self, id: EdgeId) -> Option<&Relationship> {
        self.edges.get(id.0 as usize).and_then(|e| e.as_ref())
    }

    /// Returns all neighbor node IDs reachable from `from` via `rel_type`.
    ///
    /// Merges results from the delta log and the CSR.
    pub fn get_neighbors(&self, from: NodeId, rel_type: &RelType) -> Vec<(NodeId, EdgeId)> {
        let mut result = Vec::new();

        // Check delta log
        if let Some(delta) = self.deltas.get(rel_type) {
            for &(to, edge_id) in delta.get(from) {
                if !self.deleted_nodes.contains(&to) {
                    result.push((to, edge_id));
                }
            }
        }

        // Check CSR
        if let Some(csr) = self.csr.get(rel_type) {
            let targets = csr.neighbor_targets(from);
            let edge_ids = csr.neighbor_edges(from);
            for (i, &to) in targets.iter().enumerate() {
                if !self.deleted_nodes.contains(&to) {
                    result.push((to, edge_ids[i]));
                }
            }
        }

        result
    }

    /// Returns all node IDs with the given label.
    pub fn get_nodes_by_label(&self, label: &Label) -> Vec<NodeId> {
        self.label_index
            .get(label)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|id| !self.deleted_nodes.contains(id))
            .collect()
    }

    /// Returns all node IDs (for AllNodesScan).
    pub fn get_all_nodes(&self) -> Vec<NodeId> {
        (0..self.next_node_id)
            .map(NodeId)
            .filter(|id| !self.deleted_nodes.contains(id))
            .collect()
    }

    /// Deletes a node and all its edges.
    pub fn delete_node(&mut self, id: NodeId) {
        if self.deleted_nodes.contains(&id) {
            return;
        }

        // Remove from label index
        if let Some(node) = self.nodes.get(id.0 as usize).and_then(|n| n.as_ref()) {
            for label in node.labels.iter() {
                if let Some(ids) = self.label_index.get_mut(label) {
                    ids.retain(|&nid| nid != id);
                }
            }
        }

        self.deleted_nodes.insert(id);
        self.access_tracker.remove(id);
    }

    /// Deletes an edge by ID.
    pub fn delete_edge(&mut self, id: EdgeId) {
        if let Some(Some(edge)) = self.edges.get_mut(id.0 as usize) {
            let from = edge.from;
            let _ = edge.from; // edge.from used before clearing
                               // Remove from delta logs
            for delta in self.deltas.values_mut() {
                delta.remove_edge(id);
            }
            // For CSR edges, we rely on compaction to clean up
            let _ = from;
        }
        if let Some(slot) = self.edges.get_mut(id.0 as usize) {
            *slot = None;
        }
    }

    /// Sets or updates a property on a node.
    pub fn set_property(&mut self, node_id: NodeId, key: impl Into<String>, value: Value) {
        if let Some(node) = self.get_node_mut(node_id) {
            node.properties.insert(key, value);
        }
    }

    /// Removes a property from a node.
    pub fn remove_property(&mut self, node_id: NodeId, key: &str) {
        if let Some(node) = self.get_node_mut(node_id) {
            node.properties.remove(key);
        }
    }

    /// Compacts the delta log for a relationship type into the CSR.
    ///
    /// Reads all existing CSR edges + delta edges, rebuilds the CSR,
    /// and clears the delta.
    pub fn compact(&mut self, rel_type: &RelType) {
        let mut all_edges: Vec<(NodeId, NodeId, EdgeId)> = Vec::new();

        // Collect existing CSR edges (filtering out deleted)
        if let Some(csr) = self.csr.get(rel_type) {
            for (from, to, eid) in csr.iter_edges() {
                if !self.deleted_nodes.contains(&from) && !self.deleted_nodes.contains(&to) {
                    all_edges.push((from, to, eid));
                }
            }
        }

        // Collect delta edges
        if let Some(delta) = self.deltas.get_mut(rel_type) {
            for (from, to, eid) in delta.drain() {
                if !self.deleted_nodes.contains(&from) && !self.deleted_nodes.contains(&to) {
                    all_edges.push((from, to, eid));
                }
            }
        }

        // Rebuild CSR
        self.csr.build_rel_type(rel_type.clone(), &all_edges);
    }

    /// Returns `true` if the delta for a relationship type exceeds the compaction threshold.
    pub fn needs_compaction(&self, rel_type: &RelType) -> bool {
        self.deltas
            .get(rel_type)
            .map(|d| d.len() >= self.compaction_threshold)
            .unwrap_or(false)
    }

    /// Returns all relationship types that currently have delta entries.
    pub fn dirty_rel_types(&self) -> Vec<RelType> {
        self.deltas
            .iter()
            .filter(|(_, d)| !d.is_empty())
            .map(|(rt, _)| rt.clone())
            .collect()
    }

    /// Records an access to a node (for LRU tracking).
    pub fn touch(&mut self, id: NodeId) {
        self.access_tracker.touch(id);
    }

    /// Returns eviction candidates (least recently used node IDs).
    pub fn evict_candidates(&self, count: usize) -> Vec<NodeId> {
        self.access_tracker.evict_candidates(count)
    }

    /// Removes a node from the LRU tracker (after eviction to cold tier).
    pub fn evict_node(&mut self, id: NodeId) -> Option<Node> {
        self.access_tracker.remove(id);
        self.nodes.get_mut(id.0 as usize).and_then(|n| n.take())
    }

    /// Estimates current memory usage in bytes.
    pub fn memory_usage(&self) -> usize {
        // Rough estimate: nodes + edges + adjacency
        let node_mem: usize = self
            .nodes
            .iter()
            .filter_map(|n| n.as_ref())
            .map(|n| {
                std::mem::size_of::<Node>() + n.labels.len() * 32 + n.properties.len() * 64
                // rough: key + value
            })
            .sum();

        let edge_mem: usize = self
            .edges
            .iter()
            .filter_map(|e| e.as_ref())
            .map(|e| std::mem::size_of::<Relationship>() + e.properties.len() * 64)
            .sum();

        node_mem + edge_mem
    }

    /// Returns a reference to the LRU access tracker.
    pub fn access_tracker(&self) -> &LruTracker {
        &self.access_tracker
    }

    /// Returns the next node ID to be allocated (for persistence).
    pub fn next_node_id(&self) -> u64 {
        self.next_node_id
    }

    /// Sets the next node ID (for restoration from persistence).
    pub fn set_next_node_id(&mut self, id: u64) {
        self.next_node_id = id;
    }
}

impl Default for HotTier {
    fn default() -> Self {
        HotTier::new(1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_get_node() {
        let mut tier = HotTier::new(1000);
        let id = tier.insert_node(
            LabelSet::from_iter([Label::from("Person")]),
            PropertyMap::from_iter([("name".to_string(), Value::String("Alice".into()))]),
        );

        let node = tier.get_node(id).unwrap();
        assert_eq!(node.id, id);
        assert!(node.has_label(&Label::from("Person")));
        assert_eq!(
            node.get_property("name"),
            Some(&Value::String("Alice".into()))
        );
    }

    #[test]
    fn test_insert_and_get_edge() {
        let mut tier = HotTier::new(1000);
        let a = tier.insert_node(LabelSet::new(), PropertyMap::new());
        let b = tier.insert_node(LabelSet::new(), PropertyMap::new());
        let eid = tier.insert_edge(a, b, RelType::from("KNOWS"), PropertyMap::new());

        let edge = tier.get_edge(eid).unwrap();
        assert_eq!(edge.from, a);
        assert_eq!(edge.to, b);
        assert_eq!(edge.rel_type, RelType::from("KNOWS"));
    }

    #[test]
    fn test_get_neighbors_from_delta() {
        let mut tier = HotTier::new(1000);
        let a = tier.insert_node(LabelSet::new(), PropertyMap::new());
        let b = tier.insert_node(LabelSet::new(), PropertyMap::new());
        let c = tier.insert_node(LabelSet::new(), PropertyMap::new());

        tier.insert_edge(a, b, RelType::from("KNOWS"), PropertyMap::new());
        tier.insert_edge(a, c, RelType::from("KNOWS"), PropertyMap::new());

        let neighbors = tier.get_neighbors(a, &RelType::from("KNOWS"));
        assert_eq!(neighbors.len(), 2);
        assert!(neighbors.iter().any(|(n, _)| *n == b));
        assert!(neighbors.iter().any(|(n, _)| *n == c));
    }

    #[test]
    fn test_compaction_merges_delta_into_csr() {
        let mut tier = HotTier::new(2); // low threshold for testing
        let a = tier.insert_node(LabelSet::new(), PropertyMap::new());
        let b = tier.insert_node(LabelSet::new(), PropertyMap::new());
        let c = tier.insert_node(LabelSet::new(), PropertyMap::new());

        tier.insert_edge(a, b, RelType::from("KNOWS"), PropertyMap::new());
        tier.insert_edge(a, c, RelType::from("KNOWS"), PropertyMap::new());

        assert!(tier.needs_compaction(&RelType::from("KNOWS")));

        tier.compact(&RelType::from("KNOWS"));

        // After compaction, neighbors should still be correct (now from CSR)
        let neighbors = tier.get_neighbors(a, &RelType::from("KNOWS"));
        assert_eq!(neighbors.len(), 2);
    }

    #[test]
    fn test_label_index() {
        let mut tier = HotTier::new(1000);
        let a = tier.insert_node(
            LabelSet::from_iter([Label::from("Person")]),
            PropertyMap::new(),
        );
        let b = tier.insert_node(
            LabelSet::from_iter([Label::from("Person")]),
            PropertyMap::new(),
        );
        let _c = tier.insert_node(
            LabelSet::from_iter([Label::from("Movie")]),
            PropertyMap::new(),
        );

        let persons = tier.get_nodes_by_label(&Label::from("Person"));
        assert_eq!(persons.len(), 2);
        assert!(persons.contains(&a));
        assert!(persons.contains(&b));

        let movies = tier.get_nodes_by_label(&Label::from("Movie"));
        assert_eq!(movies.len(), 1);
    }

    #[test]
    fn test_delete_node() {
        let mut tier = HotTier::new(1000);
        let id = tier.insert_node(
            LabelSet::from_iter([Label::from("Person")]),
            PropertyMap::new(),
        );

        tier.delete_node(id);
        assert!(tier.get_node(id).is_none());

        // Label index should be updated
        let persons = tier.get_nodes_by_label(&Label::from("Person"));
        assert!(persons.is_empty());
    }

    #[test]
    fn test_set_and_remove_property() {
        let mut tier = HotTier::new(1000);
        let id = tier.insert_node(LabelSet::new(), PropertyMap::new());

        tier.set_property(id, "score", Value::from(42i64));
        assert_eq!(
            tier.get_node(id).unwrap().get_property("score"),
            Some(&Value::Integer(42))
        );

        tier.remove_property(id, "score");
        assert_eq!(tier.get_node(id).unwrap().get_property("score"), None);
    }

    #[test]
    fn test_all_nodes() {
        let mut tier = HotTier::new(1000);
        let a = tier.insert_node(LabelSet::new(), PropertyMap::new());
        let b = tier.insert_node(LabelSet::new(), PropertyMap::new());
        let c = tier.insert_node(LabelSet::new(), PropertyMap::new());

        tier.delete_node(b);

        let all = tier.get_all_nodes();
        assert_eq!(all.len(), 2);
        assert!(all.contains(&a));
        assert!(all.contains(&c));
        assert!(!all.contains(&b));
    }

    #[test]
    fn test_memory_usage() {
        let mut tier = HotTier::new(1000);
        let initial = tier.memory_usage();
        tier.insert_node(LabelSet::new(), PropertyMap::new());
        assert!(tier.memory_usage() > initial);
    }
}
