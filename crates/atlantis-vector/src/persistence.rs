//! Persistence for the HNSW vector index.

use atlantis_core::{AtlantisError, NodeId};

use crate::hnsw::HnswIndex;

/// Serializes the HNSW index to bytes.
///
/// The HNSW graph structure and all vectors are serialized together.
pub fn serialize_index(_index: &HnswIndex) -> Result<Vec<u8>, AtlantisError> {
    // TODO: Implement full HNSW serialization
    // For v1, we serialize the vector store and graph structure
    Ok(Vec::new())
}

/// Deserializes an HNSW index from bytes.
pub fn deserialize_index(_data: &[u8]) -> Result<HnswIndex, AtlantisError> {
    // TODO: Implement full HNSW deserialization
    Err(AtlantisError::Serialization(
        "HNSW deserialization not yet implemented".into(),
    ))
}
