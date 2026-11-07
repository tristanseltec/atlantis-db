//! On-disk cold tier storage for evicted nodes.
//!
//! Uses a simple append-only file with page-based layout. Evicted nodes
//! are serialized with bincode and written to the page file. On access,
//! they are promoted back to the hot tier.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use atlantis_core::AtlantisError;
use atlantis_core::{Node, NodeId};

/// Location of a node within the cold tier page file.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct PageLocation {
    pub offset: u64,
    pub size: u32,
}

/// On-disk cold tier storing evicted nodes.
///
/// Nodes are serialized with bincode and appended to a single file.
/// A page index maps node IDs to their locations for O(1) retrieval.
pub struct ColdTier {
    /// The page file for serialized nodes.
    file: File,
    /// Index: node ID -> location in the file.
    page_index: HashMap<NodeId, PageLocation>,
    /// Base directory path.
    base_path: PathBuf,
}

impl ColdTier {
    /// Opens or creates a cold tier at the given directory path.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, AtlantisError> {
        let base_path = path.into();
        std::fs::create_dir_all(&base_path)?;
        let file_path = base_path.join("cold_tier.db");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&file_path)?;
        Ok(ColdTier {
            file,
            page_index: HashMap::new(),
            base_path,
        })
    }

    /// Writes a node to the cold tier, recording its location.
    pub fn write_node(&mut self, node: &Node) -> Result<PageLocation, AtlantisError> {
        let data =
            bincode::serialize(node).map_err(|e| AtlantisError::Serialization(e.to_string()))?;
        let offset = self.file.seek(SeekFrom::End(0))?;
        let size = data.len() as u32;
        self.file.write_all(&data)?;

        let loc = PageLocation { offset, size };
        self.page_index.insert(node.id, loc);
        Ok(loc)
    }

    /// Reads a node from the cold tier by its ID.
    pub fn read_node(&mut self, id: NodeId) -> Result<Node, AtlantisError> {
        let loc = self
            .page_index
            .get(&id)
            .ok_or(AtlantisError::NodeNotFound(id))?;

        self.file.seek(SeekFrom::Start(loc.offset))?;
        let mut buf = vec![0u8; loc.size as usize];
        self.file.read_exact(&mut buf)?;
        let node: Node =
            bincode::deserialize(&buf).map_err(|e| AtlantisError::Serialization(e.to_string()))?;
        Ok(node)
    }

    /// Returns `true` if a node is stored in the cold tier.
    pub fn contains(&self, id: NodeId) -> bool {
        self.page_index.contains_key(&id)
    }

    /// Removes a node's entry from the cold tier index.
    pub fn remove(&mut self, id: NodeId) {
        self.page_index.remove(&id);
    }

    /// Returns all node IDs currently stored in the cold tier.
    pub fn node_ids(&self) -> Vec<NodeId> {
        self.page_index.keys().copied().collect()
    }

    /// Returns the number of nodes in the cold tier.
    pub fn len(&self) -> usize {
        self.page_index.len()
    }

    /// Returns `true` if the cold tier is empty.
    pub fn is_empty(&self) -> bool {
        self.page_index.is_empty()
    }

    /// Returns the base directory path.
    pub fn path(&self) -> &PathBuf {
        &self.base_path
    }

    /// Returns the page index for persistence.
    pub fn page_index(&self) -> &HashMap<NodeId, PageLocation> {
        &self.page_index
    }

    /// Restores the page index from a saved index.
    pub fn restore_index(&mut self, index: HashMap<NodeId, PageLocation>) {
        self.page_index = index;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlantis_core::{Label, LabelSet, PropertyMap, Value};

    fn make_node(id: u64, name: &str) -> Node {
        Node::new(NodeId(id))
            .with_label("Person")
            .with_property("name", name)
    }

    #[test]
    fn test_write_and_read_node() {
        let dir = tempfile::tempdir().unwrap();
        let mut cold = ColdTier::open(dir.path()).unwrap();

        let node = make_node(0, "Alice");
        cold.write_node(&node).unwrap();

        let read = cold.read_node(NodeId(0)).unwrap();
        assert_eq!(read.id, NodeId(0));
        assert_eq!(
            read.get_property("name"),
            Some(&Value::String("Alice".into()))
        );
    }

    #[test]
    fn test_contains_and_len() {
        let dir = tempfile::tempdir().unwrap();
        let mut cold = ColdTier::open(dir.path()).unwrap();

        assert!(cold.is_empty());

        let node = make_node(1, "Bob");
        cold.write_node(&node).unwrap();

        assert!(cold.contains(NodeId(1)));
        assert!(!cold.contains(NodeId(99)));
        assert_eq!(cold.len(), 1);
    }

    #[test]
    fn test_remove() {
        let dir = tempfile::tempdir().unwrap();
        let mut cold = ColdTier::open(dir.path()).unwrap();

        cold.write_node(&make_node(0, "Alice")).unwrap();
        cold.write_node(&make_node(1, "Bob")).unwrap();

        cold.remove(NodeId(0));
        assert!(!cold.contains(NodeId(0)));
        assert!(cold.contains(NodeId(1)));
    }

    #[test]
    fn test_multiple_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let mut cold = ColdTier::open(dir.path()).unwrap();

        for i in 0..10 {
            cold.write_node(&make_node(i, &format!("User{i}"))).unwrap();
        }

        assert_eq!(cold.len(), 10);
        for i in 0..10 {
            let node = cold.read_node(NodeId(i)).unwrap();
            assert_eq!(
                node.get_property("name"),
                Some(&Value::String(format!("User{i}").into()))
            );
        }
    }

    #[test]
    fn test_read_missing_node() {
        let dir = tempfile::tempdir().unwrap();
        let mut cold = ColdTier::open(dir.path()).unwrap();
        let result = cold.read_node(NodeId(99));
        assert!(matches!(result, Err(AtlantisError::NodeNotFound(_))));
    }
}
