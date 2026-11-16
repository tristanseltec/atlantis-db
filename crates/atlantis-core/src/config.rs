//! Configuration types for the Atlantis DB engine.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::AtlantisError;

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

impl DistanceMetricKind {
    /// Parses `'cosine'`, `'euclidean'` or `'dot_product'` (case-insensitive).
    pub fn parse(name: &str) -> Result<Self, AtlantisError> {
        match name.to_ascii_lowercase().as_str() {
            "cosine" => Ok(DistanceMetricKind::Cosine),
            "euclidean" | "l2" => Ok(DistanceMetricKind::Euclidean),
            "dot_product" | "dot" => Ok(DistanceMetricKind::DotProduct),
            other => Err(AtlantisError::Config(format!(
                "unknown distance metric '{other}' (expected cosine, euclidean or dot_product)"
            ))),
        }
    }
}

/// When committed transactions are forced to disk.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default, Serialize, Deserialize)]
pub enum SyncMode {
    /// Every commit is fsynced to the write-ahead log before it returns or
    /// becomes visible. Concurrent commits share one fsync (group commit).
    #[default]
    Every,
    /// The log is fsynced by a background thread at this interval; commits
    /// return without waiting. A machine crash can lose the commits of the
    /// last interval.
    Interval(std::time::Duration),
    /// Commits are written to the log but left to the OS to flush. A crash
    /// of the machine (not just the process) can lose recent commits.
    Never,
}

/// Transaction isolation level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Isolation {
    /// Reads see a consistent snapshot taken at `begin`. A commit fails if
    /// another transaction committed a write to the same key after the
    /// snapshot (first committer wins). Write skew is possible.
    #[default]
    Snapshot,
    /// Snapshot isolation plus read validation: a commit also fails if any
    /// key it read, or any range it scanned, changed after the snapshot.
    Serializable,
}

/// Transaction defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransactionConfig {
    /// Isolation of read-write transactions that do not choose one:
    /// `Engine::transaction`, `transaction_with_retry` and Cypher statements.
    #[serde(default)]
    pub isolation: Isolation,
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

/// Defaults for vector indexes. `CREATE VECTOR INDEX` options override
/// these per index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorConfig {
    /// Default dimensionality when an index does not set `dimension`.
    pub dimensions: usize,
    /// Default distance metric.
    pub distance_metric: DistanceMetricKind,
    /// Default HNSW parameters.
    pub hnsw: HnswParams,
    /// Threads that build a vector index from its records; `0` (the
    /// default) uses one per available CPU.
    #[serde(default)]
    pub build_threads: usize,
}

impl VectorConfig {
    /// The number of index-build threads to use: `build_threads`, or the
    /// available parallelism when it is `0`.
    pub fn effective_build_threads(&self) -> usize {
        match self.build_threads {
            0 => std::thread::available_parallelism().map_or(1, |n| n.get()),
            n => n,
        }
    }
}

impl Default for VectorConfig {
    fn default() -> Self {
        VectorConfig {
            dimensions: 128,
            distance_metric: DistanceMetricKind::Cosine,
            hnsw: HnswParams::default(),
            build_threads: 0,
        }
    }
}

/// Storage configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Directory holding the database files (`records.kv` checkpoint,
    /// `records.wal` log, `LOCK`). `None` means in-memory only.
    pub disk_path: Option<PathBuf>,
    /// When commits are forced to disk.
    #[serde(default)]
    pub sync: SyncMode,
}

/// Top-level engine configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    /// Storage configuration.
    pub storage: StorageConfig,
    /// Vector index defaults.
    pub vector: VectorConfig,
    /// Interval in seconds between automatic checkpoints, which fold the
    /// write-ahead log into the checkpoint file. 0 = disabled.
    pub checkpoint_interval_secs: u64,
    /// Transaction defaults.
    #[serde(default)]
    pub transaction: TransactionConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            storage: StorageConfig::default(),
            vector: VectorConfig::default(),
            checkpoint_interval_secs: 300,
            transaction: TransactionConfig::default(),
        }
    }
}

impl EngineConfig {
    /// Creates a new builder for `EngineConfig`.
    pub fn builder() -> EngineConfigBuilder {
        EngineConfigBuilder::default()
    }

    /// Validates all configuration values.
    pub fn validate(&self) -> Result<(), AtlantisError> {
        if self.storage.sync == SyncMode::Interval(std::time::Duration::ZERO) {
            return Err(AtlantisError::Config(
                "the sync interval must be greater than 0".into(),
            ));
        }
        self.vector.validate()
    }
}

fn validate_positive(name: &str, value: usize) -> Result<(), AtlantisError> {
    if value == 0 {
        return Err(AtlantisError::Config(format!(
            "{name} must be greater than 0"
        )));
    }
    Ok(())
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
    /// Sets the database directory.
    pub fn disk_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.storage.disk_path = Some(path.into());
        self
    }

    /// Sets when commits are forced to disk.
    pub fn sync_mode(mut self, sync: SyncMode) -> Self {
        self.config.storage.sync = sync;
        self
    }

    /// Sets the isolation of read-write transactions that do not choose one.
    pub fn default_isolation(mut self, isolation: Isolation) -> Self {
        self.config.transaction.isolation = isolation;
        self
    }

    /// Sets the default vector dimensionality.
    pub fn vector_dimensions(mut self, dims: usize) -> Self {
        self.config.vector.dimensions = dims;
        self
    }

    /// Sets the default distance metric.
    pub fn distance_metric(mut self, metric: DistanceMetricKind) -> Self {
        self.config.vector.distance_metric = metric;
        self
    }

    /// Sets the default HNSW parameters.
    pub fn hnsw_params(mut self, params: HnswParams) -> Self {
        self.config.vector.hnsw = params;
        self
    }

    /// Sets the number of vector-index build threads (`0`: one per CPU).
    pub fn vector_build_threads(mut self, threads: usize) -> Self {
        self.config.vector.build_threads = threads;
        self
    }

    /// Sets the automatic checkpoint interval in seconds (0 disables it).
    pub fn checkpoint_interval(mut self, secs: u64) -> Self {
        self.config.checkpoint_interval_secs = secs;
        self
    }

    /// Validates and returns the configuration.
    pub fn build(self) -> Result<EngineConfig, AtlantisError> {
        self.config.validate()?;
        Ok(self.config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let config = EngineConfig::default();
        assert_eq!(config.storage.disk_path, None);
        assert_eq!(config.storage.sync, SyncMode::Every);
        assert_eq!(config.vector.dimensions, 128);
        assert_eq!(config.vector.distance_metric, DistanceMetricKind::Cosine);
        assert_eq!(config.checkpoint_interval_secs, 300);
    }

    #[test]
    fn builder_sets_fields() {
        let config = EngineConfig::builder()
            .disk_path("/tmp/x")
            .sync_mode(SyncMode::Never)
            .vector_dimensions(384)
            .distance_metric(DistanceMetricKind::Euclidean)
            .checkpoint_interval(60)
            .build()
            .unwrap();
        assert_eq!(config.storage.disk_path, Some(PathBuf::from("/tmp/x")));
        assert_eq!(config.storage.sync, SyncMode::Never);
        assert_eq!(config.vector.dimensions, 384);
        assert_eq!(config.vector.distance_metric, DistanceMetricKind::Euclidean);
        assert_eq!(config.checkpoint_interval_secs, 60);
    }

    #[test]
    fn validation() {
        assert!(EngineConfig::builder()
            .vector_dimensions(0)
            .build()
            .is_err());
        let bad = HnswParams {
            m: 16,
            m0: 8,
            ..HnswParams::default()
        };
        assert!(EngineConfig::builder().hnsw_params(bad).build().is_err());
    }

    #[test]
    fn serde_roundtrip_and_missing_sync_defaults() {
        let config = EngineConfig::builder()
            .checkpoint_interval(120)
            .build()
            .unwrap();
        let bytes = bincode::serialize(&config).unwrap();
        let back: EngineConfig = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back.checkpoint_interval_secs, 120);
    }

    #[test]
    fn metric_names() {
        assert_eq!(
            DistanceMetricKind::parse("COSINE").unwrap(),
            DistanceMetricKind::Cosine
        );
        assert_eq!(
            DistanceMetricKind::parse("dot_product").unwrap(),
            DistanceMetricKind::DotProduct
        );
        assert!(DistanceMetricKind::parse("manhattan").is_err());
    }
}
