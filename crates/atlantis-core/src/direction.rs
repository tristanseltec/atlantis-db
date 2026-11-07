//! Direction enum for relationship traversal.

use serde::{Deserialize, Serialize};

/// The direction of a relationship in a graph pattern.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Direction {
    /// Outgoing: `()-[r]->()` — from the source node to the target.
    Outgoing,
    /// Incoming: `()<-[]-()` — from the target node to the source.
    Incoming,
    /// Both: `()<-[]->()` — either direction.
    Both,
}

impl Direction {
    /// Returns `true` if this direction includes outgoing edges.
    pub fn includes_outgoing(&self) -> bool {
        matches!(self, Direction::Outgoing | Direction::Both)
    }

    /// Returns `true` if this direction includes incoming edges.
    pub fn includes_incoming(&self) -> bool {
        matches!(self, Direction::Incoming | Direction::Both)
    }

    /// Reverses the direction: Outgoing ↔ Incoming, Both stays Both.
    pub fn reverse(&self) -> Direction {
        match self {
            Direction::Outgoing => Direction::Incoming,
            Direction::Incoming => Direction::Outgoing,
            Direction::Both => Direction::Both,
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Direction::Outgoing => write!(f, "OUTGOING"),
            Direction::Incoming => write!(f, "INCOMING"),
            Direction::Both => write!(f, "BOTH"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_includes_outgoing() {
        assert!(Direction::Outgoing.includes_outgoing());
        assert!(Direction::Both.includes_outgoing());
        assert!(!Direction::Incoming.includes_outgoing());
    }

    #[test]
    fn test_includes_incoming() {
        assert!(Direction::Incoming.includes_incoming());
        assert!(Direction::Both.includes_incoming());
        assert!(!Direction::Outgoing.includes_incoming());
    }

    #[test]
    fn test_reverse() {
        assert_eq!(Direction::Outgoing.reverse(), Direction::Incoming);
        assert_eq!(Direction::Incoming.reverse(), Direction::Outgoing);
        assert_eq!(Direction::Both.reverse(), Direction::Both);
    }

    #[test]
    fn test_display() {
        assert_eq!(Direction::Outgoing.to_string(), "OUTGOING");
        assert_eq!(Direction::Incoming.to_string(), "INCOMING");
        assert_eq!(Direction::Both.to_string(), "BOTH");
    }
}
