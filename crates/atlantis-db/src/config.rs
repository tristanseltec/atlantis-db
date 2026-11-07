//! Public configuration for Atlantis DB.

pub use atlantis_core::{
    DistanceMetricKind, EngineConfig, EngineConfigBuilder, HnswParams, StorageConfig, VectorConfig,
};

/// Public configuration type — an alias for `EngineConfig`.
///
/// `Config::builder()` resolves to [`EngineConfig::builder`].
pub type Config = EngineConfig;
