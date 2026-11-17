//! Recursive-descent Cypher parser.
//!
//! [`cursor`] holds the token cursor and error helpers; each grammar layer
//! lives in its own module as an `impl Parser` block.

pub mod ast;
mod clause;
mod cursor;
mod ddl;
mod expression;
mod pattern;
mod statement;

#[cfg(test)]
mod tests;

use atlantis_core::AtlantisError;

use crate::lexer::{tokenize, Token};
use cursor::Parser;

pub use ast::*;

/// Parses exactly one statement; a single trailing `;` is allowed.
pub fn parse(input: &str) -> Result<Statement, AtlantisError> {
    let mut p = Parser::new(input, tokenize(input)?);
    let stmt = p.statement()?;
    p.eat(&Token::Semicolon);
    p.expect_end("end of input")?;
    Ok(stmt)
}

/// Parses a `;`-separated script. Empty statements are ignored.
pub fn parse_script(input: &str) -> Result<Vec<Statement>, AtlantisError> {
    let mut p = Parser::new(input, tokenize(input)?);
    let mut out = Vec::new();
    loop {
        while p.eat(&Token::Semicolon) {}
        if p.at(&Token::Eof) {
            return Ok(out);
        }
        out.push(p.statement()?);
        if !p.at(&Token::Eof) {
            p.expect(&Token::Semicolon, "';' or end of input")?;
        }
    }
}
