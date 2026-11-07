//! Tiering manager for moving data between hot and cold tiers.

use atlantis_core::{NodeId, StorageConfig};
use atlantis_core::AtlantisError;

use crate::cold_tier::ColdTier;
use crate::hot_tier::HotTier;

/// Manages the movement of data between the hot (in-memory) and cold
/// (on-disk) tiers based on the configured eviction policy.
pub struct TieringManager<'a> {
    hot: &'a mut HotTier,
    cold: &'a mut ColdTier,
    config: &'a StorageConfig,
}

impl<'a> TieringManager<'a> {
    /// Creates a new tiering manager with references to both tiers.
    pub fn new(
        hot: &'a mut HotTier,
        cold: &'a mut ColdTier,
        config: &'a StorageConfig,
    ) -> Self {
        TieringManager { hot, cold, config }
    }

    /// Checks if the hot tier exceeds the memory budget and evicts
    /// cold data to the cold tier if needed.
    ///
    /// Returns the number of nodes evicted.
    pub fn maybe_evict(&mut self) -> Result<usize, AtlantisError> {
        let budget = match self.config.memory_budget_bytes {
            Some(b) => b,
            None => return Ok(0), // No limit = no eviction
        };

        let usage = self.hot.memory_usage();
        if usage <= budget {
            return Ok(0);
        }

        // Estimate how many nodes to evict (rough: evict 10% of nodes)
        let candidates = self.hot.evict_candidates(10);
        let mut evicted = 0;

        for node_id in candidates {
            if let Some(node) = self.hot.evict_node(node_id) {
                self.cold.write_node(&node)?;
                evicted += 1;
            }
        }

        Ok(evicted)
    }

    /// Promotes a node from the cold tier back to the hot tier.
    ///
    /// Returns the node if it was found in the cold tier.
    pub fn promote(&mut self, node_id: NodeId) -> Result<Option<atlantis_core::Node>, AtlantisError> {
        if !self.cold.contains(node_id) {
            return Ok(None);
        }

        let node = self.cold.read_node(node_id)?;
        self.cold.remove(node_id);

        // Re-insert into hot tier with same ID is not straightforward (IDs are sequential).
        // For v1, we store the node data and the caller handles re-association.
        Ok(Some(node))
    }

    /// Returns `true` if a node is stored in the cold tier.
    pub fn is_on_disk(&self, node_id: NodeId) -> bool {
        self.cold.contains(node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlantis_core::{Label, LabelSet, PropertyMap};

    fn make_storage_config(budget: usize) -> StorageConfig {
        StorageConfig {
            memory_budget_bytes: Some(budget),
            eviction_policy: atlantis_core::EvictionPolicy::Lru,
            disk_path: None,
            compaction_threshold: 1000,
        }
    }

    #[test]
    fn test_no_eviction_under_budget() {
        let dir = tempfile::tempdir().unwrap();
        let mut hot = HotTier::new(1000);
        let mut cold = ColdTier::open(dir.path()).unwrap();
        let config = make_storage_config(1_000_000_000); // 1GB budget

        hot.insert_node(LabelSet::from_iter([Label::from("Test")]), PropertyMap::new());

        let mut mgr = TieringManager::new(&mut hot, &mut cold, &config);
        let evicted = mgr.maybe_evict().unwrap();
        assert_eq!(evicted, 0);
    }

    #[test]
    fn test_no_eviction_no_budget() {
        let dir = tempfile::tempdir().unwrap();
        let mut hot = HotTier::new(1000);
        let mut cold = ColdTier::open(dir.path()).unwrap();
        let config = StorageConfig {
            memory_budget_bytes: None,
            eviction_policy: atlantis_core::EvictionPolicy::Lru,
            disk_path: None,
            compaction_threshold: 1000,
        };

        hot.insert_node(LabelSet::new(), PropertyMap::new());

        let mut mgr = TieringManager::new(&mut hot, &mut cold, &config);
        let evicted = mgr.maybe_evict().unwrap();
        assert_eq!(evicted, 0);
    }

    #[test]
    fn test_promote_missing_node() {
        let dir = tempfile::tempdir().unwrap();
        let mut hot = HotTier::new(1000);
        let mut cold = ColdTier::open(dir.path()).unwrap();
        let config = make_storage_config(1_000_000_000);

        let mut mgr = TieringManager::new(&mut hot, &mut cold, &config);
        let result = mgr.promote(NodeId(99)).unwrap();
        assert!(result.is_none());
    }
}
