//! Core types and traits for Atlantis DB.
//!
//! This crate provides the data model shared across all Atlantis DB crates:
//!
//! - the unified record model: [`DatabaseRef`] (namespace + database),
//!   [`RecordId`] (table + [`RecordKey`]), [`Record`] and [`Edge`];
//! - the [`Value`] type system, including nested documents, temporal,
//!   spatial and identifier types;
//! - configuration structs and the unified error type.

pub mod config;
pub mod error;
pub mod geometry;
pub mod ids;
pub mod node;
pub mod record;
pub mod temporal;
pub mod value;

pub use config::{
    DistanceMetricKind, EngineConfig, EngineConfigBuilder, HnswParams, StorageConfig, SyncMode,
    VectorConfig,
};
pub use error::AtlantisError;
/// The target name of the unified error type; an alias for [`AtlantisError`].
pub type Error = AtlantisError;
pub use geometry::{Geometry, Point};
pub use ids::{Ulid, Uuid};
pub use node::NodeId;
pub use record::{
    validate_ident, validate_user_table, DatabaseRef, Edge, Object, Record, RecordId, RecordKey,
    DEFAULT_DATABASE, DEFAULT_NAMESPACE, DEFAULT_TABLE,
};
pub use temporal::{Datetime, Duration};
pub mod direction;
pub use direction::Direction;
pub use value::Value;
