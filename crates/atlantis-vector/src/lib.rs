//! HNSW vector index for Atlantis DB.
//!
//! Implements the Hierarchical Navigable Small World algorithm for
//! approximate nearest neighbor search, with pluggable distance metrics
//! and in-memory storage. Indexes are rebuilt from records on startup.

pub mod distance;
pub mod filter;
pub mod hnsw;
pub mod params;
pub mod vector_store;

pub use distance::{CosineDistance, DistanceMetric, DotProductDistance, EuclideanDistance};
pub use filter::{AllowSet, FilterStrategy};
pub use hnsw::HnswIndex;
pub use params::{HnswParams, SearchParams};
pub use vector_store::VectorStore;
