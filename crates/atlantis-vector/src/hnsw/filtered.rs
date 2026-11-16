//! Filtered search: exact scan over a small allow set, or a graph walk
//! that admits only allowed nodes.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::atomic::Ordering::Relaxed;

use atlantis_core::{AtlantisError, NodeId};

use crate::distance::inverse_norm;
use crate::filter::{AllowSet, FilterStrategy};

use super::scored::{Query, Scored};
use super::visited::{Heaps, Scratch, SCRATCH};
use super::{HnswIndex, NO_LEVEL};

/// Cost model of a filtered graph walk: about this many distance
/// computations per result-pool slot, divided by the filter's selectivity.
const TRAVERSAL_COST_PER_SLOT: f64 = 10.0;

impl HnswIndex {
    /// Searches for the `k` nearest neighbours among the nodes in `allow`.
    ///
    /// Picks a [`FilterStrategy`] with [`filter_strategy`](Self::filter_strategy):
    /// exact distances over the allow set when it is small, else a graph walk
    /// that traverses every node but admits only allowed ones to the results.
    /// Either way every hit is in `allow`, and recall does not collapse when
    /// few nodes match (filtering after an unfiltered search returns almost
    /// nothing at 1% selectivity).
    pub fn search_filtered(
        &self,
        query: &[f32],
        k: usize,
        ef_search: Option<usize>,
        allow: &AllowSet,
    ) -> Result<Vec<(NodeId, f32)>, AtlantisError> {
        if query.len() != self.vectors.dimensions() {
            return Err(AtlantisError::DimensionMismatch {
                expected: self.vectors.dimensions(),
                actual: query.len(),
            });
        }
        let Some((entry, max_level)) = self.entry_state() else {
            return Ok(Vec::new());
        };
        if k == 0 || allow.is_empty() {
            return Ok(Vec::new());
        }
        let ef = ef_search.unwrap_or(self.params.ef_search).max(k);
        let query = Query {
            v: query,
            inv_norm: inverse_norm(query),
        };
        let found = match self.filter_strategy(allow.len(), ef) {
            FilterStrategy::Exact => self.exact_over(&query, k, allow),
            FilterStrategy::Traversal => {
                let mut entry_points = vec![Scored {
                    dist: self.dist_to(&query, entry),
                    id: entry,
                }];
                for l in (1..=max_level).rev() {
                    entry_points = self.search_layer(&query, &entry_points, 1, l, true);
                }
                SCRATCH.with(|scratch| {
                    let mut scratch = scratch.borrow_mut();
                    scratch.visited.start(self.levels.len());
                    self.search_layer_filtered(&mut scratch, &query, &entry_points, ef, allow)
                })
            }
        };
        Ok(found
            .into_iter()
            .take(k)
            .map(|c| (NodeId(c.id as u64), self.reported(c.dist)))
            .collect())
    }

    /// The strategy [`search_filtered`](Self::search_filtered) uses for an
    /// allow set of `allowed` nodes and a result pool of `ef`.
    ///
    /// Exact search costs one distance per allowed node. A filtered walk
    /// must find about `ef` allowed nodes, so it visits about
    /// `ef / selectivity` nodes; exact wins while
    /// `allowed ≤ TRAVERSAL_COST_PER_SLOT · ef / selectivity`.
    pub fn filter_strategy(&self, allowed: usize, ef: usize) -> FilterStrategy {
        let live = self.len().max(1) as f64;
        let selectivity = (allowed as f64 / live).clamp(1.0 / live, 1.0);
        let walk = TRAVERSAL_COST_PER_SLOT * ef.max(1) as f64 / selectivity;
        if (allowed as f64) <= walk {
            FilterStrategy::Exact
        } else {
            FilterStrategy::Traversal
        }
    }

    /// Exact top-`k` over the live nodes of `allow`.
    fn exact_over(&self, query: &Query<'_>, k: usize, allow: &AllowSet) -> Vec<Scored> {
        let mut best: BinaryHeap<Scored> = BinaryHeap::with_capacity(k + 1);
        for id in allow.iter() {
            let i = id as usize;
            if i >= self.levels.len() || self.levels[i] == NO_LEVEL || self.deleted[i] {
                continue;
            }
            let dist = self.dist_to(query, id);
            if best.len() < k {
                best.push(Scored { dist, id });
            } else if best.peek().is_some_and(|w| dist < w.dist) {
                best.pop();
                best.push(Scored { dist, id });
            }
        }
        best.into_sorted_vec()
    }

    /// Layer-0 best-first search that traverses every node but keeps only
    /// live nodes in `allow` as results. The stop rule is the usual one, so
    /// the walk goes on until `ef` allowed nodes are found and no closer
    /// candidate remains.
    fn search_layer_filtered(
        &self,
        scratch: &mut Scratch,
        query: &Query<'_>,
        entry_points: &[Scored],
        ef: usize,
        allow: &AllowSet,
    ) -> Vec<Scored> {
        let Scratch { visited, heaps } = scratch;
        let ef = ef.max(1);
        let admit = |id: u32| allow.contains(id) && !self.deleted[id as usize];
        heaps.reset(ef * 4, ef + 1);
        let Heaps {
            candidates,
            results,
        } = &mut *heaps;
        for &ep in entry_points {
            if visited.insert(ep.id) {
                candidates.push(Reverse(ep));
                if admit(ep.id) {
                    results.push(ep);
                }
            }
        }
        while results.len() > ef {
            results.pop();
        }
        while let Some(Reverse(current)) = candidates.pop() {
            if results.len() >= ef && results.peek().is_some_and(|w| current.dist > w.dist) {
                break;
            }
            let links = self.links(current.id, 0);
            for (j, slot) in links.iter().enumerate() {
                let neighbor = slot.load(Relaxed);
                if let Some(next) = links.get(j + 1) {
                    self.prefetch(next.load(Relaxed));
                }
                if self.levels[neighbor as usize] == NO_LEVEL || !visited.insert(neighbor) {
                    continue;
                }
                let dist = self.dist_to(query, neighbor);
                if results.len() >= ef && results.peek().is_some_and(|w| dist >= w.dist) {
                    continue;
                }
                let scored = Scored { dist, id: neighbor };
                candidates.push(Reverse(scored));
                if admit(neighbor) {
                    results.push(scored);
                    if results.len() > ef {
                        results.pop();
                    }
                }
            }
        }
        heaps.sorted_results()
    }
}
