//! Storage engine for Atlantis DB.
//!
//! Two stores live here during the multi-model migration:
//!
//! - [`RecordStore`] — the unified record model (documents, nodes, rows and
//!   edges in namespaced tables) on top of the transactional key-value layer
//!   in `atlantis-kv`. This is the target storage design.
//! - [`GraphStore`] — the original property-graph store used by the Cypher
//!   engine: an in-memory hot tier with CSR (Compressed Sparse Row) adjacency
//!   and a delta log, an on-disk cold tier, and tiering management. It stays
//!   until the executor moves onto [`RecordStore`].

pub mod cold_tier;
pub mod csr;
pub mod delta;
pub mod hot_tier;
pub mod lru;
pub mod persistence;
pub mod records;
pub mod store;
pub mod tiering;

pub use cold_tier::{ColdTier, PageLocation};
pub use csr::CsrAdjacency;
pub use delta::DeltaLog;
pub use hot_tier::HotTier;
pub use lru::LruTracker;
pub use records::{RecordStore, RecordTx, TableDef, TableKind};
pub use store::GraphStore;
pub use tiering::TieringManager;
