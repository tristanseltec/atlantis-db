//! Token cursor shared by all grammar layers.

use atlantis_core::AtlantisError;

use crate::lexer::{line_col, SpannedToken, Token};

pub(crate) type PResult<T> = Result<T, AtlantisError>;

pub(crate) struct Parser<'a> {
    src: &'a str,
    /// Always non-empty and terminated by [`Token::Eof`].
    tokens: Vec<SpannedToken>,
    pos: usize,
}

impl<'a> Parser<'a> {
    pub(crate) fn new(src: &'a str, tokens: Vec<SpannedToken>) -> Self {
        debug_assert!(matches!(
            tokens.last(),
            Some(SpannedToken {
                token: Token::Eof,
                ..
            })
        ));
        Parser {
            src,
            tokens,
            pos: 0,
        }
    }

    pub(crate) fn peek(&self) -> &Token {
        self.peek_nth(0)
    }

    /// The token `n` positions ahead; saturates at `Eof`.
    pub(crate) fn peek_nth(&self, n: usize) -> &Token {
        let i = (self.pos + n).min(self.tokens.len() - 1);
        &self.tokens[i].token
    }

    /// Consumes and returns the current token (never moves past `Eof`).
    pub(crate) fn bump(&mut self) -> Token {
        let tok = self.tokens[self.pos].token.clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    pub(crate) fn at(&self, tok: &Token) -> bool {
        self.peek() == tok
    }

    pub(crate) fn eat(&mut self, tok: &Token) -> bool {
        let hit = self.at(tok);
        if hit {
            self.bump();
        }
        hit
    }

    pub(crate) fn expect(&mut self, tok: &Token, expected: &str) -> PResult<()> {
        if self.eat(tok) {
            Ok(())
        } else {
            Err(self.unexpected(expected))
        }
    }

    pub(crate) fn at_kw(&self, kw: &str) -> bool {
        self.peek().is_keyword(kw)
    }

    pub(crate) fn eat_kw(&mut self, kw: &str) -> bool {
        let hit = self.at_kw(kw);
        if hit {
            self.bump();
        }
        hit
    }

    pub(crate) fn expect_kw(&mut self, kw: &str) -> PResult<()> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(self.unexpected(kw))
        }
    }

    /// `true` if the token `n` ahead is a bare word or quoted identifier.
    pub(crate) fn is_name_at(&self, n: usize) -> bool {
        matches!(self.peek_nth(n), Token::Ident(_) | Token::QuotedIdent(_))
    }

    /// Any bare word (keywords included) or quoted identifier. Used where a
    /// name is unambiguous: labels, types, property keys, aliases.
    pub(crate) fn name(&mut self, expected: &str) -> PResult<String> {
        match self.peek() {
            Token::Ident(s) | Token::QuotedIdent(s) => {
                let s = s.clone();
                self.bump();
                Ok(s)
            }
            _ => Err(self.unexpected(expected)),
        }
    }

    /// Fails unless the cursor is at end of input.
    pub(crate) fn expect_end(&self, expected: &str) -> PResult<()> {
        if self.at(&Token::Eof) {
            Ok(())
        } else {
            Err(self.unexpected(expected))
        }
    }

    /// Byte offset where the current token starts.
    pub(crate) fn offset(&self) -> usize {
        self.tokens[self.pos].span.start
    }

    /// Byte offset where the previously consumed token ends.
    pub(crate) fn prev_end(&self) -> usize {
        match self.pos {
            0 => 0,
            i => self.tokens[i - 1].span.end,
        }
    }

    /// Trimmed source text between two byte offsets.
    pub(crate) fn text(&self, start: usize, end: usize) -> String {
        self.src[start..end].trim().to_string()
    }

    /// Error at the current token: `line:col: unexpected X, expected Y`.
    pub(crate) fn unexpected(&self, expected: &str) -> AtlantisError {
        self.error_at(
            self.offset(),
            &format!("unexpected {}, expected {expected}", self.peek()),
        )
    }

    pub(crate) fn error_at(&self, offset: usize, msg: &str) -> AtlantisError {
        let (line, col) = line_col(self.src, offset);
        AtlantisError::Parse(format!("{line}:{col}: {msg}"))
    }
}
