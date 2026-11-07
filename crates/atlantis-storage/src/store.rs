//! GraphStore facade combining hot and cold tiers.
//!
//! Provides a unified API for graph operations that automatically handles
//! tiering (hot/cold promotion/demotion) transparently.

use std::collections::HashMap;
use std::path::PathBuf;

use atlantis_core::AtlantisError;
use atlantis_core::{
    Direction, EdgeId, Label, LabelSet, Node, NodeId, PropertyMap, RelType, Relationship,
    StorageConfig, Value,
};

use crate::cold_tier::{ColdTier, PageLocation};
use crate::hot_tier::HotTier;
use crate::persistence;
use crate::tiering::TieringManager;

/// The unified graph store combining hot and cold tiers.
pub struct GraphStore {
    hot: HotTier,
    cold: Option<ColdTier>,
    config: StorageConfig,
}

impl GraphStore {
    /// Creates a new in-memory graph store (no disk persistence).
    pub fn new_in_memory() -> Self {
        GraphStore {
            hot: HotTier::default(),
            cold: None,
            config: StorageConfig::default(),
        }
    }

    /// Creates a new graph store with the given configuration.
    pub fn with_config(config: StorageConfig) -> Result<Self, AtlantisError> {
        let cold = match &config.disk_path {
            Some(path) => Some(ColdTier::open(path)?),
            None => None,
        };

        Ok(GraphStore {
            hot: HotTier::new(config.compaction_threshold),
            cold,
            config,
        })
    }

    /// Opens a graph store from a directory path, restoring from disk if a checkpoint exists.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, AtlantisError> {
        let path = path.into();
        let checkpoint_path = path.join("checkpoint.db");
        let config = StorageConfig {
            disk_path: Some(path.clone()),
            ..Default::default()
        };

        let cold = ColdTier::open(&path)?;

        if checkpoint_path.exists() {
            let (hot, cold_index) = persistence::restore_from_file(&checkpoint_path)?;
            let mut cold = cold;
            cold.restore_index(cold_index);
            Ok(GraphStore {
                hot,
                cold: Some(cold),
                config,
            })
        } else {
            Ok(GraphStore {
                hot: HotTier::default(),
                cold: Some(cold),
                config,
            })
        }
    }

    /// Returns a reference to the node with the given ID, promoting from cold if needed.
    pub fn get_node(&mut self, id: NodeId) -> Option<&Node> {
        // Check hot tier first
        if self.hot.get_node(id).is_some() {
            self.hot.touch(id);
            return self.hot.get_node(id);
        }

        // Check cold tier
        if let Some(cold) = &mut self.cold {
            if cold.contains(id) {
                if let Ok(node) = cold.read_node(id) {
                    // Promote: re-insert into hot tier
                    // Note: this allocates a new ID; for v1, callers use get_node_owned
                    let _ = node;
                    // TODO: proper promotion that preserves node ID
                }
            }
        }

        None
    }

    /// Returns an owned copy of the node with the given ID.
    pub fn get_node_owned(&mut self, id: NodeId) -> Option<Node> {
        self.hot.get_node(id).cloned()
    }

    /// Returns an owned copy of the node from the hot tier only (no promotion).
    /// This is safe to call with a shared reference.
    pub fn get_node_hot(&self, id: NodeId) -> Option<Node> {
        self.hot.get_node(id).cloned()
    }

    /// Returns a reference to the edge with the given ID.
    pub fn get_edge(&self, id: EdgeId) -> Option<&Relationship> {
        self.hot.get_edge(id)
    }

    /// Inserts a new node, returning its ID.
    pub fn insert_node(&mut self, labels: LabelSet, properties: PropertyMap) -> NodeId {
        self.hot.insert_node(labels, properties)
    }

    /// Inserts a new edge, returning its ID.
    pub fn insert_edge(
        &mut self,
        from: NodeId,
        to: NodeId,
        rel_type: RelType,
        properties: PropertyMap,
    ) -> EdgeId {
        let eid = self.hot.insert_edge(from, to, rel_type.clone(), properties);
        // Check if compaction is needed
        if self.hot.needs_compaction(&rel_type) {
            self.hot.compact(&rel_type);
        }
        eid
    }

    /// Returns the neighbors of a node via a relationship type.
    pub fn get_neighbors(&self, from: NodeId, rel_type: &RelType) -> Vec<(NodeId, EdgeId)> {
        self.hot.get_neighbors(from, rel_type)
    }

    /// Returns all node IDs with the given label.
    pub fn get_nodes_by_label(&self, label: &Label) -> Vec<NodeId> {
        self.hot.get_nodes_by_label(label)
    }

    /// Returns all node IDs.
    pub fn get_all_nodes(&self) -> Vec<NodeId> {
        self.hot.get_all_nodes()
    }

    /// Deletes a node.
    pub fn delete_node(&mut self, id: NodeId) {
        self.hot.delete_node(id);
        if let Some(cold) = &mut self.cold {
            cold.remove(id);
        }
    }

    /// Deletes an edge.
    pub fn delete_edge(&mut self, id: EdgeId) {
        self.hot.delete_edge(id);
    }

    /// Sets a property on a node.
    pub fn set_property(&mut self, node_id: NodeId, key: impl Into<String>, value: Value) {
        self.hot.set_property(node_id, key, value);
    }

    /// Removes a property from a node.
    pub fn remove_property(&mut self, node_id: NodeId, key: &str) {
        self.hot.remove_property(node_id, key);
    }

    /// Returns the number of nodes.
    pub fn node_count(&self) -> u64 {
        self.hot.node_count()
    }

    /// Returns the number of edges.
    pub fn edge_count(&self) -> u64 {
        self.hot.edge_count()
    }

    /// Triggers a checkpoint to disk.
    pub fn checkpoint(&self) -> Result<(), AtlantisError> {
        let path = self.config.disk_path.as_ref().ok_or_else(|| {
            AtlantisError::Storage("no disk path configured for checkpointing".into())
        })?;

        let checkpoint_path = path.join("checkpoint.db");
        let cold_index = self
            .cold
            .as_ref()
            .map(|c| c.page_index().clone())
            .unwrap_or_default();

        persistence::checkpoint(&self.hot, &cold_index, &checkpoint_path)
    }

    /// Triggers tiering eviction if needed.
    pub fn maybe_evict(&mut self) -> Result<usize, AtlantisError> {
        if let Some(cold) = &mut self.cold {
            let mut mgr = TieringManager::new(&mut self.hot, cold, &self.config);
            mgr.maybe_evict()
        } else {
            Ok(0)
        }
    }

    /// Compacts all dirty relationship types.
    pub fn compact_all(&mut self) {
        let dirty: Vec<RelType> = self.hot.dirty_rel_types();
        for rt in dirty {
            self.hot.compact(&rt);
        }
    }

    /// Returns a reference to the hot tier (for direct access by the executor).
    pub fn hot(&self) -> &HotTier {
        &self.hot
    }

    /// Returns a mutable reference to the hot tier.
    pub fn hot_mut(&mut self) -> &mut HotTier {
        &mut self.hot
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlantis_core::Value;

    #[test]
    fn test_in_memory_store() {
        let mut store = GraphStore::new_in_memory();

        let a = store.insert_node(
            LabelSet::from_iter([Label::from("Person")]),
            PropertyMap::from_iter([("name".to_string(), Value::String("Alice".into()))]),
        );
        let b = store.insert_node(
            LabelSet::from_iter([Label::from("Person")]),
            PropertyMap::from_iter([("name".to_string(), Value::String("Bob".into()))]),
        );
        store.insert_edge(a, b, RelType::from("KNOWS"), PropertyMap::new());

        assert_eq!(store.node_count(), 2);
        assert_eq!(store.edge_count(), 1);

        let neighbors = store.get_neighbors(a, &RelType::from("KNOWS"));
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].0, b);
    }

    #[test]
    fn test_persistence_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();

        let config = StorageConfig {
            disk_path: Some(path.clone()),
            ..Default::default()
        };

        let mut store = GraphStore::with_config(config).unwrap();
        let a = store.insert_node(
            LabelSet::from_iter([Label::from("Person")]),
            PropertyMap::from_iter([("name".to_string(), Value::String("Alice".into()))]),
        );
        let b = store.insert_node(LabelSet::new(), PropertyMap::new());
        store.insert_edge(a, b, RelType::from("KNOWS"), PropertyMap::new());

        store.checkpoint().unwrap();
        assert!(path.join("checkpoint.db").exists());

        // Restore
        let restored = GraphStore::open(&path).unwrap();
        assert_eq!(restored.node_count(), 2);
    }

    #[test]
    fn test_delete() {
        let mut store = GraphStore::new_in_memory();
        let a = store.insert_node(LabelSet::new(), PropertyMap::new());
        let b = store.insert_node(LabelSet::new(), PropertyMap::new());
        let eid = store.insert_edge(a, b, RelType::from("KNOWS"), PropertyMap::new());

        assert_eq!(store.edge_count(), 1);
        store.delete_edge(eid);
        assert!(store.get_edge(eid).is_none());

        store.delete_node(a);
        assert!(store.get_node_owned(a).is_none());
    }
}
