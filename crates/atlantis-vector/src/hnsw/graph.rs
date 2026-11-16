//! Link storage: reading and writing link lists under lock stripes.

use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

use super::scored::Scored;
use super::{lock, HnswIndex, LOCK_STRIPES, NO_LEVEL};

impl HnswIndex {
    /// The link slots of `node` at `layer`: the count, then `cap` ids.
    fn link_slots(&self, node: u32, layer: usize) -> &[AtomicU32] {
        let i = node as usize;
        match self.levels.get(i) {
            Some(&lvl) if lvl != NO_LEVEL && layer <= lvl as usize => {}
            _ => return &[],
        }
        if layer == 0 {
            let stride = self.params.m0 + 1;
            &self.layer0[i * stride..(i + 1) * stride]
        } else {
            let stride = self.params.m + 1;
            &self.upper[i][(layer - 1) * stride..layer * stride]
        }
    }

    /// The links of `node` at `layer`, as atomics to load.
    #[inline]
    pub(super) fn links(&self, node: u32, layer: usize) -> &[AtomicU32] {
        let slots = self.link_slots(node, layer);
        let Some((count, ids)) = slots.split_first() else {
            return &[];
        };
        let n = (count.load(Relaxed) as usize).min(ids.len());
        &ids[..n]
    }

    pub(super) fn link_ids(&self, node: u32, layer: usize) -> Vec<u32> {
        self.links(node, layer)
            .iter()
            .map(|a| a.load(Relaxed))
            .collect()
    }

    /// Writes `node`'s list at `layer`. The caller holds `node`'s stripe.
    fn store_links(&self, node: u32, layer: usize, links: &[u32]) {
        let slots = self.link_slots(node, layer);
        let Some((count, ids)) = slots.split_first() else {
            return;
        };
        let n = links.len().min(ids.len());
        for (slot, &id) in ids.iter().zip(&links[..n]) {
            slot.store(id, Relaxed);
        }
        count.store(n as u32, Relaxed);
    }

    /// Adds `new` links to `node`'s list at `layer`, shrinking it to `cap`
    /// with the heuristic when it overflows. Only `node`'s list changes, and
    /// links that other threads added meanwhile are kept as candidates.
    pub(super) fn merge_links(&self, node: u32, layer: usize, new: &[u32], cap: usize) {
        let _guard = lock(&self.stripes[node as usize % LOCK_STRIPES]);
        let mut list = self.link_ids(node, layer);
        let before = list.len();
        for &n in new {
            if n != node && !list.contains(&n) {
                list.push(n);
            }
        }
        if list.len() == before {
            return;
        }
        if list.len() <= cap {
            self.store_links(node, layer, &list);
            return;
        }
        let mut scored: Vec<Scored> = list
            .into_iter()
            .filter(|&n| !self.deleted[n as usize])
            .map(|n| Scored {
                dist: self.dist_between(node, n),
                id: n,
            })
            .collect();
        scored.sort_unstable();
        let kept = self.select_neighbors(&scored, cap);
        self.store_links(node, layer, &kept);
    }
}
