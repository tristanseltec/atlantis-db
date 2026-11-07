//! Relationship type for edge categorization.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A relationship type (e.g., `KNOWS`, `ACTED_IN`, `EMBEDS`).
///
/// Like `Label`, uses `Arc<str>` for cheap cloning — relationship types
/// are frequently shared across edges.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RelType(Arc<str>);

impl RelType {
    /// Creates a new relationship type from a string-like value.
    pub fn new(s: impl AsRef<str>) -> Self {
        RelType(Arc::from(s.as_ref()))
    }

    /// Returns the relationship type as a `&str`.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for RelType {
    fn from(s: &str) -> Self {
        RelType::new(s)
    }
}

impl From<String> for RelType {
    fn from(s: String) -> Self {
        RelType::new(s)
    }
}

impl std::fmt::Display for RelType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, ":{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rel_type_creation() {
        let r1 = RelType::from("KNOWS");
        let r2 = RelType::new("KNOWS");
        assert_eq!(r1, r2);
        assert_eq!(r1.as_str(), "KNOWS");
    }

    #[test]
    fn test_rel_type_display() {
        let r = RelType::from("ACTED_IN");
        assert_eq!(r.to_string(), ":ACTED_IN");
    }

    #[test]
    fn test_rel_type_ordering() {
        let a = RelType::from("AAA");
        let b = RelType::from("ZZZ");
        assert!(a < b);
    }

    #[test]
    fn test_rel_type_equality() {
        let a = RelType::from("EMBEDS");
        let b = RelType::from("EMBEDS");
        assert_eq!(a, b);
        assert_ne!(a, RelType::from("KNOWS"));
    }
}
