//! Cypher query language front end for Atlantis DB.
//!
//! A hand-written lexer and recursive-descent parser turn Cypher text into
//! the [`ast`] that the engine interprets clause by clause. The dialect is a
//! practical subset of openCypher plus vector-index statements.

pub mod lexer;
pub mod parser;

pub use lexer::{tokenize, SpannedToken, Token};
pub use parser::ast;
pub use parser::{parse, parse_script};
