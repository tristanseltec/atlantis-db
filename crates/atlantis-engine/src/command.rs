//! Commands sent to the core engine actor.

use std::collections::HashMap;

use atlantis_core::{AtlantisError, NodeId, Value};
use atlantis_storage::GraphStore;
use tokio::sync::{mpsc, oneshot};

/// A command sent to the core engine actor.
#[derive(Debug)]
pub enum Command {
    /// Execute a Cypher query with parameters.
    Query {
        cypher: String,
        params: HashMap<String, Value>,
        respond: oneshot::Sender<Result<QueryResult, AtlantisError>>,
    },
    /// Create a node directly (bypassing Cypher).
    CreateNode {
        labels: Vec<String>,
        properties: HashMap<String, Value>,
        respond: oneshot::Sender<Result<NodeId, AtlantisError>>,
    },
    /// Get a node by ID.
    GetNode {
        id: NodeId,
        respond: oneshot::Sender<Result<Option<Node>, AtlantisError>>,
    },
    /// Checkpoint the store to disk.
    Checkpoint {
        respond: oneshot::Sender<Result<(), AtlantisError>>,
    },
    /// Shut down the engine.
    Shutdown {
        respond: oneshot::Sender<Result<(), AtlantisError>>,
    },
}

/// Result of a query execution: column names and rows of values.
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// Column names (from RETURN items).
    pub columns: Vec<String>,
    /// Rows of values, one per result row.
    pub rows: Vec<Vec<Value>>,
}

impl QueryResult {
    /// Creates a new empty query result.
    pub fn new() -> Self {
        QueryResult {
            columns: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// Creates a query result with the given columns.
    pub fn with_columns(columns: Vec<String>) -> Self {
        QueryResult {
            columns,
            rows: Vec::new(),
        }
    }
}

impl Default for QueryResult {
    fn default() -> Self {
        QueryResult::new()
    }
}

/// Re-export for use in handle.
pub use atlantis_core::Node;
