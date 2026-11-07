//! Token types produced by the Cypher lexer.

use std::fmt;
use std::ops::Range;

/// A lexical token.
///
/// Keywords are not distinguished from identifiers here: the parser
/// recognises them case-insensitively from [`Token::Ident`], which lets
/// words such as `order` or `type` still serve as labels and property keys.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// A bare word: an identifier or a keyword.
    Ident(String),
    /// A backtick-quoted identifier; never a keyword.
    QuotedIdent(String),
    /// An unsigned integer literal. Values up to `2^63` are accepted so the
    /// parser can form `i64::MIN` from a negated literal.
    Integer(u64),
    Float(f64),
    /// A string literal with escapes already decoded.
    Str(String),
    /// `$name`
    Param(String),
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Dot,
    DotDot,
    Colon,
    Semicolon,
    Pipe,
    Plus,
    PlusEq,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Eq,
    /// `=~`
    RegexMatch,
    /// `<>` or `!=`
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    /// End of input; always the last token.
    Eof,
}

impl Token {
    /// `true` if this is the bare word `kw` (case-insensitive).
    pub fn is_keyword(&self, kw: &str) -> bool {
        matches!(self, Token::Ident(s) if s.eq_ignore_ascii_case(kw))
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sym = match self {
            Token::Ident(s) => return write!(f, "'{s}'"),
            Token::QuotedIdent(s) => return write!(f, "`{}`", s.replace('`', "``")),
            Token::Integer(n) => return write!(f, "'{n}'"),
            Token::Float(x) => return write!(f, "'{x}'"),
            Token::Str(s) => return write!(f, "string {s:?}"),
            Token::Param(s) => return write!(f, "'${s}'"),
            Token::Eof => return f.write_str("end of input"),
            Token::LParen => "(",
            Token::RParen => ")",
            Token::LBracket => "[",
            Token::RBracket => "]",
            Token::LBrace => "{",
            Token::RBrace => "}",
            Token::Comma => ",",
            Token::Dot => ".",
            Token::DotDot => "..",
            Token::Colon => ":",
            Token::Semicolon => ";",
            Token::Pipe => "|",
            Token::Plus => "+",
            Token::PlusEq => "+=",
            Token::Minus => "-",
            Token::Star => "*",
            Token::Slash => "/",
            Token::Percent => "%",
            Token::Caret => "^",
            Token::Eq => "=",
            Token::RegexMatch => "=~",
            Token::NotEq => "<>",
            Token::Lt => "<",
            Token::LtEq => "<=",
            Token::Gt => ">",
            Token::GtEq => ">=",
        };
        write!(f, "'{sym}'")
    }
}

/// A token with its byte range in the source text.
#[derive(Debug, Clone, PartialEq)]
pub struct SpannedToken {
    pub token: Token,
    pub span: Range<usize>,
}

/// One-based `(line, column)` of a byte offset; columns count characters.
pub fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(src.len());
    let before = &src[..offset];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let col = before[line_start..].chars().count() + 1;
    (line, col)
}
