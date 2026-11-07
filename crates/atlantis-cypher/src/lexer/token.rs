//! Token types for the Cypher lexer.

use serde::{Deserialize, Serialize};

/// A lexical token in the Cypher query language.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Token {
    // Keywords
    Match,
    Optional,
    Where,
    Return,
    Create,
    Merge,
    Delete,
    Detach,
    Set,
    Remove,
    With,
    Order,
    By,
    Limit,
    Skip,
    And,
    Or,
    Not,
    Xor,
    Call,
    Yield,
    Distinct,
    As,
    On,
    Union,
    All,
    Unwind,
    In,
    Is,
    Null,
    True,
    False,
    Starts,
    Ends,
    Contains,

    // Punctuation
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    Dot,
    Pipe,
    Equals,
    NotEquals,
    Lt,
    Gt,
    LtEq,
    GtEq,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    ArrowLeft,
    ArrowRight,
    ArrowBoth,

    // Literals
    Integer(i64),
    Float(f64),
    String(String),
    Identifier(String),

    // Special
    Parameter(String),
    Eof,
}

/// A token with its source span (byte offset range).
#[derive(Debug, Clone, PartialEq)]
pub struct SpannedToken {
    pub token: Token,
    pub span: std::ops::Range<usize>,
}

impl Token {
    /// Returns `true` if this token is a keyword.
    pub fn is_keyword(&self) -> bool {
        !matches!(
            self,
            Token::Integer(_)
                | Token::Float(_)
                | Token::String(_)
                | Token::Identifier(_)
                | Token::Parameter(_)
                | Token::Eof
                | Token::LParen
                | Token::RParen
                | Token::LBrace
                | Token::RBrace
                | Token::LBracket
                | Token::RBracket
                | Token::Colon
                | Token::Comma
                | Token::Dot
                | Token::Pipe
                | Token::Equals
                | Token::NotEquals
                | Token::Lt
                | Token::Gt
                | Token::LtEq
                | Token::GtEq
                | Token::Plus
                | Token::Minus
                | Token::Star
                | Token::Slash
                | Token::Percent
                | Token::ArrowLeft
                | Token::ArrowRight
                | Token::ArrowBoth
        )
    }
}
