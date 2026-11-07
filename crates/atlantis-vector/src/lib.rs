//! HNSW vector index for Atlantis DB.
//!
//! Implements the Hierarchical Navigable Small World algorithm for
//! approximate nearest neighbor search, with pluggable distance metrics
//! and support for persistence.

pub mod distance;
pub mod hnsw;
pub mod params;
pub mod persistence;
pub mod vector_store;

pub use distance::{CosineDistance, DistanceMetric, DotProductDistance, EuclideanDistance};
pub use hnsw::HnswIndex;
pub use params::{HnswParams, SearchParams};
pub use vector_store::VectorStore;
