//! Storage engine for Atlantis DB.
//!
//! [`RecordStore`] implements the unified record model — documents, graph
//! nodes, rows and edges in namespaced tables — on top of the transactional
//! key-value layer in `atlantis-kv`. Edges keep adjacency entries on both
//! endpoints, so graph traversal is a prefix scan.

pub mod records;

pub use records::{CommitInfo, RecordStore, RecordTx, TableDef, TableKind, VectorIndexDef};
