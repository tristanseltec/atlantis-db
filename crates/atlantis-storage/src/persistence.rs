//! Persistence format for the graph store.
//!
//! Serializes the entire hot tier (nodes, edges, CSR, label index) to
//! a binary file using bincode. On startup, the file is read and the
//! in-memory state is reconstructed.

use std::collections::HashMap;
use std::path::Path;

use atlantis_core::AtlantisError;
use atlantis_core::{EdgeId, Label, Node, NodeId, RelType, Relationship, StorageConfig};

use crate::cold_tier::PageLocation;
use crate::hot_tier::HotTier;

/// Serialized graph data for persistence.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SerializedStore {
    pub magic: [u8; 8],
    pub version: u32,
    pub nodes: Vec<Node>,
    pub edges: Vec<Relationship>,
    pub label_index: HashMap<Label, Vec<NodeId>>,
    pub next_node_id: u64,
    pub next_edge_id: u64,
    pub cold_index: HashMap<NodeId, PageLocation>,
}

const MAGIC: [u8; 8] = *b"ATLANTIS";
const FORMAT_VERSION: u32 = 1;

/// Serializes the hot tier's current state to a byte vector.
pub fn serialize_hot_tier(hot: &HotTier, cold_index: &HashMap<NodeId, PageLocation>) -> Vec<u8> {
    let nodes: Vec<Node> = hot
        .get_all_nodes()
        .into_iter()
        .filter_map(|id| hot.get_node(id).cloned())
        .collect();

    // Collect all edges
    let edges: Vec<Relationship> = (0..hot.edge_count())
        .map(EdgeId)
        .filter_map(|eid| hot.get_edge(eid).cloned())
        .collect();

    // Collect label index
    let mut label_index: HashMap<Label, Vec<NodeId>> = HashMap::new();
    for node in &nodes {
        for label in node.labels.iter() {
            label_index.entry(label.clone()).or_default().push(node.id);
        }
    }

    let store = SerializedStore {
        magic: MAGIC,
        version: FORMAT_VERSION,
        nodes,
        edges,
        label_index,
        next_node_id: hot.next_node_id(),
        next_edge_id: hot.edge_count(),
        cold_index: cold_index.clone(),
    };

    bincode::serialize(&store).unwrap_or_default()
}

/// Deserializes a byte vector into a serialized store structure.
pub fn deserialize_store(data: &[u8]) -> Result<SerializedStore, AtlantisError> {
    let store: SerializedStore =
        bincode::deserialize(data).map_err(|e| AtlantisError::Serialization(e.to_string()))?;

    if store.magic != MAGIC {
        return Err(AtlantisError::Storage(
            "invalid file format: bad magic bytes".into(),
        ));
    }

    if store.version != FORMAT_VERSION {
        return Err(AtlantisError::Storage(format!(
            "unsupported format version: {} (expected {})",
            store.version, FORMAT_VERSION
        )));
    }

    Ok(store)
}

/// Writes the serialized store to a file at the given path.
pub fn checkpoint(
    hot: &HotTier,
    cold_index: &HashMap<NodeId, PageLocation>,
    path: &Path,
) -> Result<(), AtlantisError> {
    let data = serialize_hot_tier(hot, cold_index);
    std::fs::write(path, &data)?;
    Ok(())
}

/// Restores a hot tier from a serialized store.
pub fn restore(data: &[u8]) -> Result<(HotTier, HashMap<NodeId, PageLocation>), AtlantisError> {
    let store = deserialize_store(data)?;

    let mut hot = HotTier::new(1000);

    // Restore nodes
    for node in store.nodes {
        hot.insert_node(node.labels, node.properties);
    }

    // Restore edges
    for edge in store.edges {
        hot.insert_edge(edge.from, edge.to, edge.rel_type, edge.properties);
    }

    Ok((hot, store.cold_index))
}

/// Restores from a file path.
pub fn restore_from_file(
    path: &Path,
) -> Result<(HotTier, HashMap<NodeId, PageLocation>), AtlantisError> {
    let data = std::fs::read(path)?;
    restore(&data)
}
