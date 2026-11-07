//! Core types and traits for Atlantis DB.
//!
//! This crate provides the fundamental data model types shared across all
//! Atlantis DB crates: `Value`, `Node`, `Relationship`, `Label`, `RelType`,
//! `PropertyMap`, configuration structs, and the unified error type.

pub mod config;
pub mod error;
pub mod label;
pub mod node;
pub mod property;
pub mod rel_type;
pub mod relationship;
pub mod value;

pub use config::{
    DistanceMetricKind, EngineConfig, EngineConfigBuilder, EvictionPolicy, HnswParams,
    StorageConfig, VectorConfig,
};
pub use error::AtlantisError;
pub use label::{Label, LabelSet};
pub use node::{Node, NodeId};
pub mod direction;
pub use direction::Direction;
pub use property::PropertyMap;
pub use rel_type::RelType;
pub use relationship::{EdgeId, Relationship};
pub use value::Value;
