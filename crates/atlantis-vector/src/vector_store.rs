//! Compact vector storage for HNSW index.

use serde::{Deserialize, Serialize};

use atlantis_core::{AtlantisError, NodeId};

/// Compact storage for vectors with pre-allocation.
///
/// All vectors share the same dimensionality. Vectors are stored in a
/// contiguous `Vec<Vec<f32>>` for cache-friendly access during HNSW search.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VectorStore {
    vectors: Vec<Vec<f32>>,
    dimensions: usize,
}

impl VectorStore {
    /// Creates a new vector store with the given dimensionality.
    pub fn new(dimensions: usize) -> Self {
        VectorStore {
            vectors: Vec::new(),
            dimensions,
        }
    }

    /// Creates a new vector store with pre-allocated capacity.
    pub fn with_capacity(dimensions: usize, capacity: usize) -> Self {
        VectorStore {
            vectors: Vec::with_capacity(capacity),
            dimensions,
        }
    }

    /// Inserts a vector, returning its ID (sequential).
    ///
    /// Returns an error if the vector dimensionality doesn't match.
    pub fn insert(&mut self, vector: Vec<f32>) -> Result<NodeId, AtlantisError> {
        if vector.len() != self.dimensions {
            return Err(AtlantisError::DimensionMismatch {
                expected: self.dimensions,
                actual: vector.len(),
            });
        }
        let id = NodeId(self.vectors.len() as u64);
        self.vectors.push(vector);
        Ok(id)
    }

    /// Inserts a vector at a specific ID (for restoring from persistence).
    pub fn insert_at(&mut self, id: NodeId, vector: Vec<f32>) -> Result<(), AtlantisError> {
        if vector.len() != self.dimensions {
            return Err(AtlantisError::DimensionMismatch {
                expected: self.dimensions,
                actual: vector.len(),
            });
        }
        let idx = id.0 as usize;
        if idx >= self.vectors.len() {
            self.vectors.resize(idx + 1, Vec::new());
        }
        self.vectors[idx] = vector;
        Ok(())
    }

    /// Returns the vector with the given ID.
    pub fn get(&self, id: NodeId) -> Option<&[f32]> {
        self.vectors.get(id.0 as usize).map(|v| v.as_slice())
    }

    /// Removes a vector (sets to empty for tombstone-based deletion).
    pub fn remove(&mut self, id: NodeId) {
        if let Some(slot) = self.vectors.get_mut(id.0 as usize) {
            slot.clear();
        }
    }

    /// Returns `true` if the vector at this ID exists and is non-empty.
    pub fn contains(&self, id: NodeId) -> bool {
        self.vectors
            .get(id.0 as usize)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Returns the number of active (non-empty) vectors.
    pub fn len(&self) -> usize {
        self.vectors.iter().filter(|v| !v.is_empty()).count()
    }

    /// Returns `true` if there are no vectors.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the dimensionality of stored vectors.
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Returns the total capacity (including empty slots).
    pub fn capacity(&self) -> usize {
        self.vectors.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_and_get() {
        let mut store = VectorStore::new(3);
        let id = store.insert(vec![1.0, 2.0, 3.0]).unwrap();

        assert_eq!(id, NodeId(0));
        assert_eq!(store.get(id), Some([1.0, 2.0, 3.0].as_ref()));
    }

    #[test]
    fn test_dimension_mismatch() {
        let mut store = VectorStore::new(3);
        let result = store.insert(vec![1.0, 2.0]);
        assert!(matches!(
            result,
            Err(AtlantisError::DimensionMismatch { .. })
        ));
    }

    #[test]
    fn test_multiple_inserts() {
        let mut store = VectorStore::new(2);
        let id0 = store.insert(vec![1.0, 0.0]).unwrap();
        let id1 = store.insert(vec![0.0, 1.0]).unwrap();

        assert_eq!(id0, NodeId(0));
        assert_eq!(id1, NodeId(1));
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn test_remove() {
        let mut store = VectorStore::new(2);
        let id = store.insert(vec![1.0, 2.0]).unwrap();
        assert!(store.contains(id));

        store.remove(id);
        assert!(!store.contains(id));
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn test_empty_store() {
        let store = VectorStore::new(128);
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        assert_eq!(store.dimensions(), 128);
    }

    #[test]
    fn test_with_capacity() {
        let store = VectorStore::with_capacity(64, 1000);
        assert_eq!(store.dimensions(), 64);
        assert_eq!(store.capacity(), 0); // nothing inserted yet
    }

    #[test]
    fn test_insert_at() {
        let mut store = VectorStore::new(2);
        store.insert_at(NodeId(5), vec![1.0, 2.0]).unwrap();
        assert_eq!(store.get(NodeId(5)), Some([1.0, 2.0].as_ref()));
        assert!(store.contains(NodeId(5)));
    }

    #[test]
    fn test_serde_roundtrip() {
        let mut store = VectorStore::new(3);
        store.insert(vec![0.1, 0.2, 0.3]).unwrap();
        store.insert(vec![0.4, 0.5, 0.6]).unwrap();

        let serialized = bincode::serialize(&store).unwrap();
        let deserialized: VectorStore = bincode::deserialize(&serialized).unwrap();
        assert_eq!(deserialized.dimensions(), 3);
        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized.get(NodeId(0)), Some([0.1, 0.2, 0.3].as_ref()));
    }
}
