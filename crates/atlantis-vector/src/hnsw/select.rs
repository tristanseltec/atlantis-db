//! Neighbour selection heuristic.

use super::scored::Scored;
use super::HnswIndex;

impl HnswIndex {
    /// Chooses up to `m` neighbours from `candidates` (sorted closest first)
    /// with the diversity heuristic (Algorithm 4): a candidate is kept only if
    /// it is closer to the base than to every neighbour kept so far, so links
    /// point in different directions and reach other clusters. Remaining
    /// slots are filled with the closest discarded candidates
    /// (`keepPrunedConnections`), so nodes keep their full degree.
    pub(super) fn select_neighbors(&self, candidates: &[Scored], m: usize) -> Vec<u32> {
        let mut kept: Vec<u32> = Vec::with_capacity(m);
        let mut discarded: Vec<u32> = Vec::new();
        for &c in candidates {
            if kept.len() >= m {
                break;
            }
            if kept.contains(&c.id) || discarded.contains(&c.id) {
                continue;
            }
            let diverse = kept.iter().all(|&k| c.dist < self.dist_between(c.id, k));
            if diverse {
                kept.push(c.id);
            } else {
                discarded.push(c.id);
            }
        }
        for id in discarded {
            if kept.len() >= m {
                break;
            }
            kept.push(id);
        }
        kept
    }
}
