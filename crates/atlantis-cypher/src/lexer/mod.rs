//! Cypher lexer.

pub mod token;
pub mod tokenizer;

pub use token::{line_col, SpannedToken, Token};
pub use tokenizer::tokenize;
