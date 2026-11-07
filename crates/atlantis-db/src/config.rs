//! Public configuration for Atlantis DB.

pub use atlantis_core::{DistanceMetricKind, EngineConfig, EngineConfigBuilder, HnswParams, StorageConfig, VectorConfig};

/// Public configuration type — an alias for `EngineConfig`.
pub type Config = EngineConfig;

impl Config {
    /// Creates a new configuration builder.
    pub fn builder() -> EngineConfigBuilder {
        EngineConfig::builder()
    }
}
