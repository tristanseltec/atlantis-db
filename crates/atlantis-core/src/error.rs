//! Error types for Atlantis DB.

use crate::node::NodeId;

/// The unified error type for all Atlantis DB operations.
#[derive(Debug, thiserror::Error)]
pub enum AtlantisError {
    /// A Cypher query failed to parse.
    #[error("parse error: {0}")]
    Parse(String),

    /// A query is syntactically valid but semantically incorrect.
    #[error("invalid query: {0}")]
    InvalidQuery(String),

    /// A referenced node was not found.
    #[error("node not found: {0:?}")]
    NodeNotFound(NodeId),

    /// An internal storage error.
    #[error("storage error: {0}")]
    Storage(String),

    /// A vector index error.
    #[error("vector error: {0}")]
    Vector(String),

    /// A vector dimension mismatch during insert or search.
    #[error("dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch { expected: usize, actual: usize },

    /// An I/O error from the filesystem.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A serialization or deserialization error.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// A configuration error.
    #[error("configuration error: {0}")]
    Config(String),

    /// The engine has been shut down and cannot process requests.
    #[error("engine shutdown")]
    Shutdown,

    /// A channel communication error (receiver dropped).
    #[error("channel error: {0}")]
    Channel(String),

    /// A value could not be parsed or is out of range.
    #[error("invalid value: {0}")]
    InvalidValue(String),

    /// A namespace, database, table, label or field name is invalid.
    #[error("{0}")]
    InvalidIdentifier(String),

    /// A record id or record key is malformed.
    #[error("invalid record id: {0}")]
    InvalidRecordId(String),

    /// A record with this id already exists.
    #[error("record already exists: {0}")]
    RecordExists(String),

    /// The referenced record does not exist.
    #[error("record not found: {0}")]
    RecordNotFound(String),

    /// A write violates a table definition or constraint.
    #[error("schema violation: {0}")]
    SchemaViolation(String),

    /// The transaction conflicted with a concurrent commit. Safe to retry.
    #[error("transaction conflict: {0}")]
    TxConflict(String),

    /// The transaction was already committed or cancelled.
    #[error("transaction already finished")]
    TxFinished,

    /// A write was attempted in a read-only transaction.
    #[error("cannot write in a read-only transaction")]
    TxReadOnly,

    /// Stored bytes could not be encoded or decoded.
    #[error("encoding error: {0}")]
    Encoding(String),
}

impl AtlantisError {
    /// Returns `true` if retrying the whole transaction may succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(self, AtlantisError::TxConflict(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let e = AtlantisError::Parse("unexpected token".into());
        assert!(e.to_string().contains("parse error"));
        assert!(e.to_string().contains("unexpected token"));
    }

    #[test]
    fn test_dimension_mismatch_display() {
        let e = AtlantisError::DimensionMismatch {
            expected: 128,
            actual: 64,
        };
        assert!(e.to_string().contains("128"));
        assert!(e.to_string().contains("64"));
    }

    #[test]
    fn test_io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err: AtlantisError = io_err.into();
        assert!(matches!(err, AtlantisError::Io(_)));
    }
}
