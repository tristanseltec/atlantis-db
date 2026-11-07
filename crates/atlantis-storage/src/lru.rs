//! LRU access tracker for tiering decisions.

use std::collections::{HashMap, VecDeque};

use atlantis_core::NodeId;

/// Tracks node access order for LRU-based eviction.
///
/// When the hot tier exceeds its memory budget, the least recently accessed
/// nodes are selected for eviction to the cold tier.
#[derive(Debug, Clone, Default)]
pub struct LruTracker {
    /// Access queue — most recent at the back, least recent at the front.
    access_order: VecDeque<NodeId>,
    /// Quick lookup: position of each node in the access_order deque.
    positions: HashMap<NodeId, usize>,
}

impl LruTracker {
    /// Creates a new empty LRU tracker.
    pub fn new() -> Self {
        LruTracker::default()
    }

    /// Records an access to the given node, moving it to the most-recently-used position.
    pub fn touch(&mut self, node: NodeId) {
        // Remove from current position if present.
        if let Some(&pos) = self.positions.get(&node) {
            self.access_order.remove(pos);
            self.rebuild_positions();
        }
        // Add to the back (most recent).
        self.access_order.push_back(node);
        self.positions.insert(node, self.access_order.len() - 1);
    }

    /// Returns the least-recently-used node IDs for eviction, up to `count` nodes.
    ///
    /// The returned nodes are not removed from the tracker — call `remove` for each.
    pub fn evict_candidates(&self, count: usize) -> Vec<NodeId> {
        self.access_order.iter().copied().take(count).collect()
    }

    /// Removes a node from the tracker (called after eviction or deletion).
    pub fn remove(&mut self, node: NodeId) {
        if let Some(&pos) = self.positions.get(&node) {
            self.access_order.remove(pos);
            self.rebuild_positions();
        }
    }

    /// Returns the number of tracked nodes.
    pub fn len(&self) -> usize {
        self.access_order.len()
    }

    /// Returns `true` if no nodes are tracked.
    pub fn is_empty(&self) -> bool {
        self.access_order.is_empty()
    }

    /// Clears all tracking data.
    pub fn clear(&mut self) {
        self.access_order.clear();
        self.positions.clear();
    }

    /// Rebuilds the position index after a removal.
    fn rebuild_positions(&mut self) {
        self.positions.clear();
        for (i, node) in self.access_order.iter().enumerate() {
            self.positions.insert(*node, i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_touch_and_evict() {
        let mut tracker = LruTracker::new();
        tracker.touch(NodeId(1));
        tracker.touch(NodeId(2));
        tracker.touch(NodeId(3));

        // LRU order: 1 (oldest), 2, 3 (newest)
        let candidates = tracker.evict_candidates(2);
        assert_eq!(candidates, vec![NodeId(1), NodeId(2)]);
    }

    #[test]
    fn test_touch_moves_to_recent() {
        let mut tracker = LruTracker::new();
        tracker.touch(NodeId(1));
        tracker.touch(NodeId(2));
        tracker.touch(NodeId(3));

        // Touch node 1 again — it should now be most recent
        tracker.touch(NodeId(1));

        // LRU order: 2, 3, 1
        let candidates = tracker.evict_candidates(1);
        assert_eq!(candidates, vec![NodeId(2)]);
    }

    #[test]
    fn test_remove() {
        let mut tracker = LruTracker::new();
        tracker.touch(NodeId(1));
        tracker.touch(NodeId(2));
        tracker.touch(NodeId(3));

        tracker.remove(NodeId(2));
        assert_eq!(tracker.len(), 2);

        // LRU order: 1, 3
        let candidates = tracker.evict_candidates(1);
        assert_eq!(candidates, vec![NodeId(1)]);
    }

    #[test]
    fn test_empty() {
        let tracker = LruTracker::new();
        assert!(tracker.is_empty());
        assert!(tracker.evict_candidates(10).is_empty());
    }

    #[test]
    fn test_clear() {
        let mut tracker = LruTracker::new();
        tracker.touch(NodeId(1));
        tracker.touch(NodeId(2));
        tracker.clear();
        assert!(tracker.is_empty());
    }
}
