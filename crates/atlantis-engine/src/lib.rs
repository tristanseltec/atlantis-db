//! Actor-based database engine for Atlantis DB.
//!
//! Provides a tokio actor-based runtime that owns the storage engine, vector
//! index, and query executor. All operations are serialized through a single
//! core actor, with background actors for tiering and checkpointing.

pub mod actor;
pub mod checkpoint_actor;
pub mod command;
pub mod executor;
pub mod handle;
pub mod tiering_actor;

pub use actor::{spawn_engine, CoreEngineActor};
pub use command::Command;
pub use executor::{QueryExecutor, Row};
pub use handle::EngineHandle;
