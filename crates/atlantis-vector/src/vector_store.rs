//! Compact vector storage for the HNSW index.

use serde::{Deserialize, Serialize};

use atlantis_core::{AtlantisError, NodeId};

/// Vectors of one dimensionality in a single contiguous `f32` arena.
///
/// Slot `i` occupies `data[i * dim .. (i + 1) * dim]`, so a graph search reads
/// neighbouring vectors without chasing a pointer per vector. Ids are slot
/// numbers; inserting at a sparse id leaves empty slots before it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VectorStore {
    data: Vec<f32>,
    present: Vec<bool>,
    live: usize,
    dimensions: usize,
}

impl VectorStore {
    /// Creates a new vector store with the given dimensionality.
    pub fn new(dimensions: usize) -> Self {
        VectorStore {
            dimensions,
            ..Default::default()
        }
    }

    /// Creates a new vector store with room for `capacity` vectors.
    pub fn with_capacity(dimensions: usize, capacity: usize) -> Self {
        VectorStore {
            data: Vec::with_capacity(dimensions * capacity),
            present: Vec::with_capacity(capacity),
            live: 0,
            dimensions,
        }
    }

    fn check(&self, vector: &[f32]) -> Result<(), AtlantisError> {
        if vector.len() != self.dimensions {
            return Err(AtlantisError::DimensionMismatch {
                expected: self.dimensions,
                actual: vector.len(),
            });
        }
        Ok(())
    }

    /// Inserts a vector, returning its ID (the next slot).
    ///
    /// Returns an error if the vector dimensionality doesn't match.
    pub fn insert(&mut self, vector: Vec<f32>) -> Result<NodeId, AtlantisError> {
        self.check(&vector)?;
        let id = NodeId(self.present.len() as u64);
        self.write(id.0 as usize, &vector);
        Ok(id)
    }

    /// Inserts (or overwrites) a vector at a specific ID.
    pub fn insert_at(&mut self, id: NodeId, vector: Vec<f32>) -> Result<(), AtlantisError> {
        self.check(&vector)?;
        self.write(id.0 as usize, &vector);
        Ok(())
    }

    pub(crate) fn write(&mut self, idx: usize, vector: &[f32]) {
        if idx >= self.present.len() {
            self.present.resize(idx + 1, false);
            self.data.resize((idx + 1) * self.dimensions, 0.0);
        }
        let d = self.dimensions;
        self.data[idx * d..(idx + 1) * d].copy_from_slice(vector);
        if !self.present[idx] {
            self.present[idx] = true;
            self.live += 1;
        }
    }

    /// Returns the vector with the given ID.
    #[inline]
    pub fn get(&self, id: NodeId) -> Option<&[f32]> {
        let idx = id.0 as usize;
        if !*self.present.get(idx)? {
            return None;
        }
        Some(self.slot(idx))
    }

    /// The vector in slot `idx`, present or not (an empty slot reads as
    /// zeros). For callers that already know the slot is present.
    #[inline]
    pub(crate) fn slot(&self, idx: usize) -> &[f32] {
        let d = self.dimensions;
        &self.data[idx * d..(idx + 1) * d]
    }

    /// Removes a vector; its slot stays allocated and reads as absent.
    pub fn remove(&mut self, id: NodeId) {
        if let Some(p) = self.present.get_mut(id.0 as usize) {
            if *p {
                *p = false;
                self.live -= 1;
            }
        }
    }

    /// Returns `true` if a vector is stored under this ID.
    #[inline]
    pub fn contains(&self, id: NodeId) -> bool {
        self.present.get(id.0 as usize).copied().unwrap_or(false)
    }

    /// Returns the number of stored vectors.
    pub fn len(&self) -> usize {
        self.live
    }

    /// Returns `true` if there are no vectors.
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Returns the dimensionality of stored vectors.
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Returns the number of slots (including empty ones).
    pub fn capacity(&self) -> usize {
        self.present.len()
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
