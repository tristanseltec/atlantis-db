//! Configuration types for the Atlantis DB engine.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The distance metric used by the HNSW vector index.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default, Serialize, Deserialize)]
pub enum DistanceMetricKind {
    /// Cosine similarity (normalized dot product). Default for embeddings.
    #[default]
    Cosine,
    /// Euclidean (L2) distance.
    Euclidean,
    /// Raw dot product (no normalization).
    DotProduct,
}

/// Eviction policy for the tiered storage manager.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default, Serialize, Deserialize)]
pub enum EvictionPolicy {
    /// Least Recently Used — evict the node that was accessed longest ago.
    #[default]
    Lru,
    /// Least Frequently Used — evict the node with the fewest accesses.
    Lfu,
}

/// HNSW index parameters.
///
/// Based on the original HNSW paper recommendations:
/// - `M`: connections per insertion. Paper suggests 5–48. Default: 16.
/// - `M0`: max connections at layer 0 = `2 * M`. Default: 32.
/// - `ef_construction`: candidate pool during insertion. Default: 200.
/// - `ef_search`: candidate pool during search. Default: 50.
/// - `max_level`: hard cap on layer count. Default: 16.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HnswParams {
    /// Number of nearest-neighbors to connect a new entry to on insertion.
    pub m: usize,
    /// Maximum connections at layer 0. Should be `2 * m`.
    pub m0: usize,
    /// Candidate pool size during index construction.
    pub ef_construction: usize,
    /// Candidate pool size during search. Higher = better recall, slower search.
    pub ef_search: usize,
    /// Hard cap on the number of HNSW layers.
    pub max_level: usize,
}

impl Default for HnswParams {
    fn default() -> Self {
        HnswParams {
            m: 16,
            m0: 32,
            ef_construction: 200,
            ef_search: 50,
            max_level: 16,
        }
    }
}

/// Configuration for the vector index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorConfig {
    /// Dimensionality of vectors stored in the index.
    pub dimensions: usize,
    /// Distance metric for similarity computation.
    pub distance_metric: DistanceMetricKind,
    /// HNSW algorithm parameters.
    pub hnsw: HnswParams,
}

impl Default for VectorConfig {
    fn default() -> Self {
        VectorConfig {
            dimensions: 128,
            distance_metric: DistanceMetricKind::Cosine,
            hnsw: HnswParams::default(),
        }
    }
}

/// Configuration for the hybrid tiered storage engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Maximum memory the hot tier may consume before eviction kicks in.
    /// `None` means no limit (purely in-memory). Default: 1 GB.
    pub memory_budget_bytes: Option<usize>,
    /// Eviction policy for moving data from hot to cold tier.
    pub eviction_policy: EvictionPolicy,
    /// Path for on-disk cold tier storage. `None` means in-memory only.
    pub disk_path: Option<PathBuf>,
    /// Number of delta edges per relationship type that triggers CSR compaction.
    pub compaction_threshold: usize,
}

impl Default for StorageConfig {
    fn default() -> Self {
        StorageConfig {
            memory_budget_bytes: Some(1024 * 1024 * 1024), // 1 GB
            eviction_policy: EvictionPolicy::Lru,
            disk_path: None,
            compaction_threshold: 1000,
        }
    }
}

/// Top-level engine configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    /// Storage configuration.
    pub storage: StorageConfig,
    /// Vector index configuration.
    pub vector: VectorConfig,
    /// Interval in seconds between automatic checkpoints. 0 = disabled.
    pub checkpoint_interval_secs: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            storage: StorageConfig::default(),
            vector: VectorConfig::default(),
            checkpoint_interval_secs: 300,
        }
    }
}

impl EngineConfig {
    /// Creates a builder for constructing an `EngineConfig` with overrides.
    pub fn builder() -> EngineConfigBuilder {
        EngineConfigBuilder::default()
    }

    /// Validates all configuration values, returning an error if any are invalid.
    ///
    /// This should be called before opening an engine to fail fast on
    /// misconfiguration rather than encountering errors at runtime.
    pub fn validate(&self) -> Result<(), AtlantisError> {
        self.storage.validate()?;
        self.vector.validate()?;
        Ok(())
    }
}

/// Validates a positive, non-zero usize field.
fn validate_positive(name: &str, value: usize) -> Result<(), AtlantisError> {
    if value == 0 {
        return Err(AtlantisError::Config(format!("{name} must be greater than 0")));
    }
    Ok(())
}

impl StorageConfig {
    /// Validates storage configuration values.
    pub fn validate(&self) -> Result<(), AtlantisError> {
        if let Some(budget) = self.memory_budget_bytes {
            if budget == 0 {
                return Err(AtlantisError::Config(
                    "memory_budget_bytes must be greater than 0 if set".into(),
                ));
            }
        }
        validate_positive("compaction_threshold", self.compaction_threshold)?;
        Ok(())
    }
}

impl VectorConfig {
    /// Validates vector configuration values.
    pub fn validate(&self) -> Result<(), AtlantisError> {
        validate_positive("dimensions", self.dimensions)?;
        self.hnsw.validate()?;
        Ok(())
    }
}

impl HnswParams {
    /// Validates HNSW parameter values.
    pub fn validate(&self) -> Result<(), AtlantisError> {
        validate_positive("m", self.m)?;
        validate_positive("m0", self.m0)?;
        validate_positive("ef_construction", self.ef_construction)?;
        validate_positive("ef_search", self.ef_search)?;
        validate_positive("max_level", self.max_level)?;
        if self.m0 < self.m {
            return Err(AtlantisError::Config(format!(
                "m0 ({}) must be >= m ({})",
                self.m0, self.m
            )));
        }
        Ok(())
    }
}

/// Builder for `EngineConfig`.
#[derive(Debug, Clone, Default)]
pub struct EngineConfigBuilder {
    config: EngineConfig,
}

impl EngineConfigBuilder {
    /// Sets the memory budget for the hot tier in bytes.
    pub fn memory_budget(mut self, bytes: usize) -> Self {
        self.config.storage.memory_budget_bytes = Some(bytes);
        self
    }

    /// Sets the disk path for persistent storage.
    pub fn disk_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.storage.disk_path = Some(path.into());
        self
    }

    /// Sets the vector dimensions.
    pub fn vector_dimensions(mut self, dims: usize) -> Self {
        self.config.vector.dimensions = dims;
        self
    }

    /// Sets the distance metric.
    pub fn distance_metric(mut self, metric: DistanceMetricKind) -> Self {
        self.config.vector.distance_metric = metric;
        self
    }

    /// Sets the HNSW parameters.
    pub fn hnsw_params(mut self, params: HnswParams) -> Self {
        self.config.vector.hnsw = params;
        self
    }

    /// Sets the eviction policy for tiered storage.
    pub fn eviction_policy(mut self, policy: EvictionPolicy) -> Self {
        self.config.storage.eviction_policy = policy;
        self
    }

    /// Sets the compaction threshold (delta edges per rel type before CSR compaction).
    pub fn compaction_threshold(mut self, threshold: usize) -> Self {
        self.config.storage.compaction_threshold = threshold;
        self
    }

    /// Sets the checkpoint interval in seconds.
    pub fn checkpoint_interval(mut self, secs: u64) -> Self {
        self.config.checkpoint_interval_secs = secs;
        self
    }

    /// Builds the final configuration, validating all values.
    ///
    /// Returns an error if any configuration value is invalid.
    pub fn build(self) -> Result<EngineConfig, AtlantisError> {
        self.config.validate()?;
        Ok(self.config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hnsw_defaults() {
        let params = HnswParams::default();
        assert_eq!(params.m, 16);
        assert_eq!(params.m0, 32);
        assert_eq!(params.ef_construction, 200);
        assert_eq!(params.ef_search, 50);
        assert_eq!(params.max_level, 16);
        assert_eq!(params.m0, params.m * 2);
    }

    #[test]
    fn test_vector_config_defaults() {
        let config = VectorConfig::default();
        assert_eq!(config.dimensions, 128);
        assert_eq!(config.distance_metric, DistanceMetricKind::Cosine);
    }

    #[test]
    fn test_storage_config_defaults() {
        let config = StorageConfig::default();
        assert_eq!(config.memory_budget_bytes, Some(1024 * 1024 * 1024));
        assert_eq!(config.eviction_policy, EvictionPolicy::Lru);
        assert_eq!(config.compaction_threshold, 1000);
        assert_eq!(config.disk_path, None);
    }

    #[test]
    fn test_engine_config_defaults() {
        let config = EngineConfig::default();
        assert_eq!(config.checkpoint_interval_secs, 300);
    }

    #[test]
    fn test_config_builder() {
        let config = EngineConfig::builder()
            .memory_budget(512 * 1024 * 1024)
            .disk_path("/tmp/atlantis")
            .vector_dimensions(256)
            .distance_metric(DistanceMetricKind::Euclidean)
            .eviction_policy(EvictionPolicy::Lfu)
            .compaction_threshold(500)
            .checkpoint_interval(60)
            .build()
            .unwrap();

        assert_eq!(config.storage.memory_budget_bytes, Some(512 * 1024 * 1024));
        assert_eq!(
            config.storage.disk_path,
            Some(PathBuf::from("/tmp/atlantis"))
        );
        assert_eq!(config.vector.dimensions, 256);
        assert_eq!(config.vector.distance_metric, DistanceMetricKind::Euclidean);
        assert_eq!(config.storage.eviction_policy, EvictionPolicy::Lfu);
        assert_eq!(config.storage.compaction_threshold, 500);
        assert_eq!(config.checkpoint_interval_secs, 60);
    }

    #[test]
    fn test_config_serde_roundtrip() {
        let config = EngineConfig::builder()
            .vector_dimensions(64)
            .checkpoint_interval(120)
            .build()
            .unwrap();

        let serialized = bincode::serialize(&config).unwrap();
        let deserialized: EngineConfig = bincode::deserialize(&serialized).unwrap();
        assert_eq!(deserialized.vector.dimensions, 64);
        assert_eq!(deserialized.checkpoint_interval_secs, 120);
    }

    #[test]
    fn test_config_validate_ok() {
        let config = EngineConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validate_zero_dimensions() {
        let mut config = EngineConfig::default();
        config.vector.dimensions = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validate_zero_m() {
        let mut config = EngineConfig::default();
        config.vector.hnsw.m = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validate_m0_less_than_m() {
        let mut config = EngineConfig::default();
        config.vector.hnsw.m = 32;
        config.vector.hnsw.m0 = 16;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validate_zero_budget() {
        let mut config = EngineConfig::default();
        config.storage.memory_budget_bytes = Some(0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validate_zero_compaction_threshold() {
        let mut config = EngineConfig::default();
        config.storage.compaction_threshold = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_config_validate_no_budget_ok() {
        let mut config = EngineConfig::default();
        config.storage.memory_budget_bytes = None;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_eviction_policy_default() {
        assert_eq!(EvictionPolicy::default(), EvictionPolicy::Lru);
    }

    #[test]
    fn test_distance_metric_default() {
        assert_eq!(DistanceMetricKind::default(), DistanceMetricKind::Cosine);
    }
}
