//! Node types for the property graph model.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::label::{Label, LabelSet};
use crate::property::PropertyMap;

/// A unique identifier for a graph node.
///
/// Internally a `u64` that is allocated sequentially by the storage engine.
/// Implements `Copy` for cheap passing by value.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeId(pub u64);

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}", self.0)
    }
}

impl From<u64> for NodeId {
    fn from(v: u64) -> Self {
        NodeId(v)
    }
}

impl From<NodeId> for u64 {
    fn from(id: NodeId) -> Self {
        id.0
    }
}

/// A graph node with labels and properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    /// The unique identifier assigned by the storage engine.
    pub id: NodeId,
    /// Zero or more labels (e.g., `:Person`, `:Embedding`).
    pub labels: LabelSet,
    /// Key-value property map.
    pub properties: PropertyMap,
}

impl Node {
    /// Creates a new node with the given ID, no labels, and no properties.
    pub fn new(id: NodeId) -> Self {
        Node {
            id,
            labels: LabelSet::new(),
            properties: PropertyMap::new(),
        }
    }

    /// Adds a label to this node, returning the node for chaining.
    pub fn with_label(mut self, label: impl Into<Label>) -> Self {
        self.labels.insert(label.into());
        self
    }

    /// Adds a property to this node, returning the node for chaining.
    pub fn with_property(mut self, key: impl Into<String>, value: impl Into<crate::Value>) -> Self {
        self.properties.insert(key, value.into());
        self
    }

    /// Returns `true` if the node has the given label.
    pub fn has_label(&self, label: &Label) -> bool {
        self.labels.contains(label)
    }

    /// Returns the property value for the given key, if present.
    pub fn get_property(&self, key: &str) -> Option<&crate::Value> {
        self.properties.get(key)
    }
}

/// Lightweight reference to a node for passing between actors
/// without cloning the full property map.
#[derive(Debug, Clone)]
pub struct NodeRef {
    pub id: NodeId,
    pub labels: Arc<LabelSet>,
}

impl NodeRef {
    pub fn from_node(node: &Node) -> Self {
        NodeRef {
            id: node.id,
            labels: Arc::new(node.labels.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;

    #[test]
    fn test_node_id_display() {
        let id = NodeId(42);
        assert_eq!(id.to_string(), "#42");
    }

    #[test]
    fn test_node_id_from_u64() {
        let id = NodeId::from(99);
        assert_eq!(id, NodeId(99));
    }

    #[test]
    fn test_node_id_to_u64() {
        let id = NodeId(42);
        let val: u64 = id.into();
        assert_eq!(val, 42);
    }

    #[test]
    fn test_node_builder() {
        let node = Node::new(NodeId(1))
            .with_label("Person")
            .with_label("Adult")
            .with_property("name", "Alice")
            .with_property("age", 30i64);

        assert_eq!(node.id, NodeId(1));
        assert!(node.has_label(&Label::from("Person")));
        assert!(node.has_label(&Label::from("Adult")));
        assert!(!node.has_label(&Label::from("Child")));
        assert_eq!(
            node.get_property("name"),
            Some(&Value::String("Alice".into()))
        );
        assert_eq!(node.get_property("age"), Some(&Value::Integer(30)));
        assert_eq!(node.get_property("missing"), None);
    }

    #[test]
    fn test_node_new_empty() {
        let node = Node::new(NodeId(0));
        assert!(node.labels.is_empty());
        assert!(node.properties.is_empty());
    }

    #[test]
    fn test_node_ref_from_node() {
        let node = Node::new(NodeId(5)).with_label("Movie");
        let node_ref = NodeRef::from_node(&node);
        assert_eq!(node_ref.id, NodeId(5));
        assert!(node_ref.labels.contains(&Label::from("Movie")));
    }

    #[test]
    fn test_node_serde_roundtrip() {
        let node = Node::new(NodeId(7))
            .with_label("Person")
            .with_property("name", "Bob")
            .with_property("scores", vec![1.0f32, 2.0, 3.0]);

        let serialized = bincode::serialize(&node).unwrap();
        let deserialized: Node = bincode::deserialize(&serialized).unwrap();
        assert_eq!(deserialized.id, node.id);
        assert!(deserialized.has_label(&Label::from("Person")));
        assert_eq!(
            deserialized.get_property("name"),
            Some(&Value::String("Bob".into()))
        );
    }
}
