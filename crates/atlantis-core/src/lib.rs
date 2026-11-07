//! Core types and traits for Atlantis DB.
//!
//! This crate provides the data model shared across all Atlantis DB crates:
//!
//! - the unified record model: [`DatabaseRef`] (namespace + database),
//!   [`RecordId`] (table + [`RecordKey`]), [`Record`] and [`Edge`];
//! - the [`Value`] type system, including nested documents, temporal,
//!   spatial and identifier types;
//! - the legacy property-graph types (`Node`, `Relationship`, `Label`,
//!   `RelType`, `PropertyMap`) still used by the Cypher engine;
//! - configuration structs and the unified error type.

pub mod config;
pub mod error;
pub mod geometry;
pub mod ids;
pub mod label;
pub mod node;
pub mod property;
pub mod record;
pub mod rel_type;
pub mod relationship;
pub mod temporal;
pub mod value;

pub use config::{
    DistanceMetricKind, EngineConfig, EngineConfigBuilder, EvictionPolicy, HnswParams,
    StorageConfig, VectorConfig,
};
pub use error::AtlantisError;
/// The target name of the unified error type; an alias for [`AtlantisError`].
pub type Error = AtlantisError;
pub use geometry::{Geometry, Point};
pub use ids::{Ulid, Uuid};
pub use label::{Label, LabelSet};
pub use node::{Node, NodeId};
pub use record::{
    validate_ident, validate_user_table, DatabaseRef, Edge, Object, Record, RecordId, RecordKey,
    DEFAULT_DATABASE, DEFAULT_NAMESPACE, DEFAULT_TABLE,
};
pub use temporal::{Datetime, Duration};
pub mod direction;
pub use direction::Direction;
pub use property::PropertyMap;
pub use rel_type::RelType;
pub use relationship::{EdgeId, Relationship};
pub use value::Value;
