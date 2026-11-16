//! Search candidates: a scored node and a prepared query.

use std::cmp::Ordering;

/// A node and its distance to the current query, ordered by distance
/// (then id, so equal distances order deterministically).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Scored {
    pub(super) dist: f32,
    pub(super) id: u32,
}

impl Eq for Scored {}

impl PartialOrd for Scored {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Scored {
    fn cmp(&self, other: &Self) -> Ordering {
        self.dist
            .total_cmp(&other.dist)
            .then(self.id.cmp(&other.id))
    }
}

/// A query vector with its inverse norm, computed once per search.
pub(super) struct Query<'a> {
    pub(super) v: &'a [f32],
    pub(super) inv_norm: f32,
}
