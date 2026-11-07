//! Cypher query language parser and planner for Atlantis DB.
//!
//! Implements a Cypher-compatible query language using chumsky parser
//! combinators. Supports a practical subset of Cypher plus vector extension
//! functions (`db.vector.search`, `db.vector.similarity`).

pub mod lexer;
pub mod parser;
pub mod planner;

pub use lexer::{tokenize, SpannedToken, Token};
pub use parser::{ast, parse};
pub use planner::{optimize, plan, LogicalOperator};
