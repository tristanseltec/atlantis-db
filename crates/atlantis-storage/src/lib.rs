//! Storage engine for Atlantis DB.
//!
//! [`RecordStore`] implements the unified record model — documents, graph
//! nodes, rows and edges in namespaced tables — on top of the transactional
//! key-value layer in `atlantis-kv`. Edges keep adjacency entries on both
//! endpoints, so graph traversal is a prefix scan.

pub mod records;

pub use records::{
    display_path, field_at, CommitInfo, ConstraintDef, ConstraintKind, FieldPath, IndexInfo,
    IndexLookup, PropertyIndexDef, RecordStore, RecordTx, SchemaDef, TableDef, TableKind,
    VectorIndexDef,
};
