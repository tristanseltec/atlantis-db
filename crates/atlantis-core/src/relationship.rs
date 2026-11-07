//! Relationship (edge) types for the property graph model.

use serde::{Deserialize, Serialize};

use crate::node::NodeId;
use crate::property::PropertyMap;
use crate::rel_type::RelType;

/// A unique identifier for a graph relationship (edge).
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EdgeId(pub u64);

impl std::fmt::Display for EdgeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#e{}", self.0)
    }
}

impl From<u64> for EdgeId {
    fn from(v: u64) -> Self {
        EdgeId(v)
    }
}

impl From<EdgeId> for u64 {
    fn from(id: EdgeId) -> Self {
        id.0
    }
}

/// A directed relationship between two nodes with a type and properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relationship {
    /// The unique identifier assigned by the storage engine.
    pub id: EdgeId,
    /// The source node of this directed edge.
    pub from: NodeId,
    /// The destination node of this directed edge.
    pub to: NodeId,
    /// The relationship type (e.g., `KNOWS`, `ACTED_IN`).
    pub rel_type: RelType,
    /// Key-value property map.
    pub properties: PropertyMap,
}

impl Relationship {
    /// Creates a new relationship with the given endpoints and type.
    pub fn new(id: EdgeId, from: NodeId, to: NodeId, rel_type: RelType) -> Self {
        Relationship {
            id,
            from,
            to,
            rel_type,
            properties: PropertyMap::new(),
        }
    }

    /// Adds a property to this relationship, returning the relationship for chaining.
    pub fn with_property(mut self, key: impl Into<String>, value: impl Into<crate::Value>) -> Self {
        self.properties.insert(key, value.into());
        self
    }

    /// Returns the property value for the given key, if present.
    pub fn get_property(&self, key: &str) -> Option<&crate::Value> {
        self.properties.get(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;

    #[test]
    fn test_edge_id_display() {
        let id = EdgeId(42);
        assert_eq!(id.to_string(), "#e42");
    }

    #[test]
    fn test_edge_id_conversions() {
        let id = EdgeId::from(99);
        assert_eq!(id, EdgeId(99));
        let val: u64 = id.into();
        assert_eq!(val, 99);
    }

    #[test]
    fn test_relationship_builder() {
        let rel = Relationship::new(EdgeId(1), NodeId(0), NodeId(1), RelType::from("KNOWS"))
            .with_property("since", 2020i64)
            .with_property("strength", 0.95f64);

        assert_eq!(rel.id, EdgeId(1));
        assert_eq!(rel.from, NodeId(0));
        assert_eq!(rel.to, NodeId(1));
        assert_eq!(rel.rel_type, RelType::from("KNOWS"));
        assert_eq!(rel.get_property("since"), Some(&Value::Integer(2020)));
        assert_eq!(rel.get_property("strength"), Some(&Value::Float(0.95)));
        assert_eq!(rel.get_property("missing"), None);
    }

    #[test]
    fn test_relationship_serde_roundtrip() {
        let rel = Relationship::new(
            EdgeId(10),
            NodeId(3),
            NodeId(7),
            RelType::from("ACTED_IN"),
        )
        .with_property("role", "Neo");

        let serialized = bincode::serialize(&rel).unwrap();
        let deserialized: Relationship = bincode::deserialize(&serialized).unwrap();
        assert_eq!(deserialized.id, rel.id);
        assert_eq!(deserialized.from, rel.from);
        assert_eq!(deserialized.to, rel.to);
        assert_eq!(deserialized.rel_type, rel.rel_type);
        assert_eq!(
            deserialized.get_property("role"),
            Some(&Value::String("Neo".into()))
        );
    }
}
