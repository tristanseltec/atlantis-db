//! HNSW algorithm parameters.

use serde::{Deserialize, Serialize};

/// Re-export the HNSW parameters from atlantis-core.
pub use atlantis_core::HnswParams;

/// Configuration for a single HNSW search operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SearchParams {
    /// Number of nearest neighbors to return.
    pub k: usize,
    /// Candidate pool size for this search (overrides default ef_search).
    /// If `None`, uses the index's default `ef_search`.
    pub ef_search: Option<usize>,
    /// Minimum similarity score threshold (0.0 to 1.0).
    /// Results below this threshold are filtered out.
    pub score_threshold: Option<f32>,
}

impl Default for SearchParams {
    fn default() -> Self {
        SearchParams {
            k: 10,
            ef_search: None,
            score_threshold: None,
        }
    }
}

impl SearchParams {
    /// Creates search params for top-k results.
    pub fn top_k(k: usize) -> Self {
        SearchParams {
            k,
            ..Default::default()
        }
    }

    /// Sets the score threshold.
    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.score_threshold = Some(threshold);
        self
    }

    /// Sets the ef_search for this query.
    pub fn with_ef_search(mut self, ef: usize) -> Self {
        self.ef_search = Some(ef);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_params() {
        let params = HnswParams::default();
        assert_eq!(params.m, 16);
        assert_eq!(params.m0, 32);
        assert_eq!(params.ef_construction, 200);
        assert_eq!(params.ef_search, 50);
    }

    #[test]
    fn test_search_params() {
        let sp = SearchParams::top_k(5).with_threshold(0.8).with_ef_search(100);
        assert_eq!(sp.k, 5);
        assert_eq!(sp.score_threshold, Some(0.8));
        assert_eq!(sp.ef_search, Some(100));
    }

    #[test]
    fn test_search_params_default() {
        let sp = SearchParams::default();
        assert_eq!(sp.k, 10);
        assert_eq!(sp.score_threshold, None);
        assert_eq!(sp.ef_search, None);
    }
}
