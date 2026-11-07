//! Hybrid tiered storage engine for Atlantis DB.
//!
//! Provides in-memory hot tier storage using CSR (Compressed Sparse Row)
//! adjacency with a delta log for incremental updates, an on-disk cold tier
//! for evicted data, and automatic tiering management.

pub mod cold_tier;
pub mod csr;
pub mod delta;
pub mod hot_tier;
pub mod lru;
pub mod persistence;
pub mod store;
pub mod tiering;

pub use cold_tier::{ColdTier, PageLocation};
pub use csr::CsrAdjacency;
pub use delta::DeltaLog;
pub use hot_tier::HotTier;
pub use lru::LruTracker;
pub use store::GraphStore;
pub use tiering::TieringManager;
