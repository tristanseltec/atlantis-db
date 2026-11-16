//! Unfiltered k-NN search and the per-layer best-first search.

use std::cmp::Reverse;
use std::sync::atomic::Ordering::Relaxed;

use atlantis_core::{AtlantisError, NodeId};

use crate::distance::inverse_norm;
use crate::params::SearchParams;

use super::scored::{Query, Scored};
use super::visited::{Heaps, Scratch, SCRATCH};
use super::{HnswIndex, NO_LEVEL};

impl HnswIndex {
    /// Searches for the k nearest neighbors of the query vector.
    ///
    /// Returns a sorted list of `(NodeId, f32)` where the float is the distance
    /// (lower = more similar).
    pub fn search(
        &self,
        query: &[f32],
        k: usize,
        ef_search: Option<usize>,
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
        let ef = ef_search.unwrap_or(self.params.ef_search).max(k);
        let query = Query {
            v: query,
            inv_norm: inverse_norm(query),
        };

        let mut entry_points = vec![Scored {
            dist: self.dist_to(&query, entry),
            id: entry,
        }];
        for l in (1..=max_level).rev() {
            entry_points = self.search_layer(&query, &entry_points, 1, l, true);
        }
        Ok(self
            .search_layer(&query, &entry_points, ef, 0, false)
            .into_iter()
            .take(k)
            .map(|c| (NodeId(c.id as u64), self.reported(c.dist)))
            .collect())
    }

    /// Searches with score threshold filtering.
    pub fn search_with_params(
        &self,
        query: &[f32],
        params: &SearchParams,
    ) -> Result<Vec<(NodeId, f32)>, AtlantisError> {
        let mut results = self.search(query, params.k, params.ef_search)?;

        if let Some(threshold) = params.score_threshold {
            // For cosine distance, similarity = 1 - distance
            // threshold is similarity, so keep results where 1 - distance >= threshold
            results.retain(|(_, dist)| 1.0 - dist >= threshold);
        }

        Ok(results)
    }

    /// Best-first search of one layer from `entry_points`, keeping the `ef`
    /// closest nodes found (Algorithm 2). The result is sorted, closest first.
    ///
    /// Deleted nodes are always traversed. With `include_deleted` they are
    /// also returned, which the descent through upper layers and insertion
    /// need: a deleted entry point must still hand the search down a layer.
    /// The final layer-0 search of a query passes `false`, so tombstones do
    /// not take result slots.
    pub(super) fn search_layer(
        &self,
        query: &Query<'_>,
        entry_points: &[Scored],
        ef: usize,
        layer: usize,
        include_deleted: bool,
    ) -> Vec<Scored> {
        SCRATCH.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            scratch.visited.start(self.levels.len());
            self.search_layer_with(
                &mut scratch,
                query,
                entry_points,
                ef,
                layer,
                include_deleted,
            )
        })
    }

    fn search_layer_with(
        &self,
        scratch: &mut Scratch,
        query: &Query<'_>,
        entry_points: &[Scored],
        ef: usize,
        layer: usize,
        include_deleted: bool,
    ) -> Vec<Scored> {
        let Scratch { visited, heaps } = scratch;
        let ef = ef.max(1);
        // Min-heap of nodes to expand; max-heap of the best `ef` found.
        heaps.reset(ef * 2, ef + 1);
        let Heaps {
            candidates,
            results,
        } = &mut *heaps;

        for &ep in entry_points {
            if visited.insert(ep.id) {
                candidates.push(Reverse(ep));
                if include_deleted || !self.deleted[ep.id as usize] {
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
            let links = self.links(current.id, layer);
            for (j, slot) in links.iter().enumerate() {
                let neighbor = slot.load(Relaxed);
                if let Some(next) = links.get(j + 1) {
                    self.prefetch(next.load(Relaxed));
                }
                if self.levels[neighbor as usize] == NO_LEVEL || !visited.insert(neighbor) {
                    continue;
                }
                let dist = self.dist_to(query, neighbor);
                let full = results.len() >= ef;
                if full && results.peek().is_some_and(|w| dist >= w.dist) {
                    continue;
                }
                let scored = Scored { dist, id: neighbor };
                candidates.push(Reverse(scored));
                if include_deleted || !self.deleted[neighbor as usize] {
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
