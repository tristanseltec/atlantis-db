//! Numeric identifier used by the vector index for its entries.

use serde::{Deserialize, Serialize};

/// A dense numeric identifier. The HNSW index stores vectors under these
/// ids; the engine maps them to record ids.
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
