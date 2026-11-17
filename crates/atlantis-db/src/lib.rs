//! Atlantis DB — an embedded multi-model database for AI applications.
//!
//! Data lives in a **namespace → database → table → record** hierarchy.
//! A record is a nested document identified by a [`RecordId`]
//! (`table:key`). Graph nodes are records whose table is their first label,
//! and relationships are [`Edge`] records stored in edge tables. Records are
//! read and written in ACID transactions:
//!
//! ```no_run
//! use atlantis_db::{Engine, Object, Value};
//!
//! # async fn example() -> Result<(), atlantis_db::Error> {
//! let engine = Engine::open("./my_db").await?;
//! let order = engine.transaction(|tx| {
//!     let mut doc = Object::new();
//!     doc.insert("total".into(), Value::Float(99.5));
//!     tx.create("Order", None, vec![], doc) // key: a generated ULID
//! }).await?;
//! println!("created {}", order.id);
//! engine.checkpoint().await?;
//! # Ok(())
//! # }
//! ```
//!
//! The same data is queried with Cypher through [`Engine::execute`] or
//! [`Transaction::execute`]: `MATCH (o:Order) RETURN o` reads the records
//! created above, and nodes created by Cypher are records in the table named
//! by their first label.

pub mod config;
pub mod engine;
pub mod params;
pub mod result_set;

pub use atlantis_core::{
    AtlantisError, DatabaseRef, Datetime, Direction, DistanceMetricKind, Duration, Edge, Error,
    Geometry, Object, Point, Record, RecordId, RecordKey, SyncMode, Ulid, Uuid, Value,
};
pub use atlantis_kv::{Isolation, Versionstamp};
pub use atlantis_storage::{
    CommitInfo, ConstraintDef, ConstraintKind, IndexInfo, IndexLookup, PropertyIndexDef, RecordTx,
    SchemaDef, TableDef, TableKind, VectorIndexDef,
};
pub use config::Config;
pub use engine::{Engine, Transaction};
pub use params::Params;
pub use result_set::{ResultSet, Row};
