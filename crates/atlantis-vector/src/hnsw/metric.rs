//! Distances under the index's metric, and vector prefetch.

use atlantis_core::DistanceMetricKind;

use crate::distance::{cosine_from_dot, dot, inverse_norm, l2_squared};

use super::scored::Query;
use super::HnswIndex;

impl HnswIndex {
    /// The distance between two vectors under this index's metric, as
    /// reported by [`search`](Self::search).
    pub fn distance(&self, a: &[f32], b: &[f32]) -> f32 {
        let d = match self.metric {
            DistanceMetricKind::Cosine => {
                cosine_from_dot(dot(a, b), inverse_norm(a), inverse_norm(b))
            }
            DistanceMetricKind::Euclidean => l2_squared(a, b),
            DistanceMetricKind::DotProduct => -dot(a, b),
        };
        self.reported(d)
    }

    /// Internal distance from a query to a stored node. Euclidean stays
    /// squared here (same order, no square root); see [`Self::reported`].
    #[inline]
    pub(super) fn dist_to(&self, query: &Query<'_>, node: u32) -> f32 {
        let v = self.vectors.slot(node as usize);
        match self.metric {
            DistanceMetricKind::Cosine => cosine_from_dot(
                dot(query.v, v),
                query.inv_norm,
                self.inv_norms[node as usize],
            ),
            DistanceMetricKind::Euclidean => l2_squared(query.v, v),
            DistanceMetricKind::DotProduct => -dot(query.v, v),
        }
    }

    /// Internal distance between two stored nodes.
    #[inline]
    pub(super) fn dist_between(&self, a: u32, b: u32) -> f32 {
        let (va, vb) = (self.vectors.slot(a as usize), self.vectors.slot(b as usize));
        match self.metric {
            DistanceMetricKind::Cosine => cosine_from_dot(
                dot(va, vb),
                self.inv_norms[a as usize],
                self.inv_norms[b as usize],
            ),
            DistanceMetricKind::Euclidean => l2_squared(va, vb),
            DistanceMetricKind::DotProduct => -dot(va, vb),
        }
    }

    /// Converts an internal distance to the one callers see.
    #[inline]
    pub(super) fn reported(&self, d: f32) -> f32 {
        match self.metric {
            DistanceMetricKind::Euclidean => d.sqrt(),
            _ => d,
        }
    }

    /// Hints the CPU to start loading `node`'s vector.
    #[inline]
    pub(super) fn prefetch(&self, node: u32) {
        #[cfg(target_arch = "x86_64")]
        {
            let v = self.vectors.slot(node as usize);
            // SAFETY: prefetching is a hint and never faults; SSE is part of
            // the x86-64 baseline.
            unsafe {
                use std::arch::x86_64::{_mm_prefetch, _MM_HINT_T0};
                _mm_prefetch::<_MM_HINT_T0>(v.as_ptr() as *const i8);
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = node;
    }
}
