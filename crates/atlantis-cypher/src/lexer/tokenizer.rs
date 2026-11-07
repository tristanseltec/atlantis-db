//! Cypher tokenizer: turns source text into spanned tokens.

use atlantis_core::AtlantisError;

use super::token::{line_col, SpannedToken, Token};

/// Largest integer literal magnitude: `2^63`, the magnitude of `i64::MIN`.
const MAX_INT_MAGNITUDE: u64 = 1 << 63;

/// Tokenizes a Cypher query. The returned vector always ends with
/// [`Token::Eof`].
pub fn tokenize(input: &str) -> Result<Vec<SpannedToken>, AtlantisError> {
    Lexer { src: input, pos: 0 }.run()
}

struct Lexer<'a> {
    src: &'a str,
    /// Current byte offset.
    pos: usize,
}

impl Lexer<'_> {
    fn run(mut self) -> Result<Vec<SpannedToken>, AtlantisError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_trivia()?;
            let start = self.pos;
            let Some(c) = self.peek() else {
                tokens.push(SpannedToken {
                    token: Token::Eof,
                    span: start..start,
                });
                return Ok(tokens);
            };
            let token = self.token(c, start)?;
            tokens.push(SpannedToken {
                token,
                span: start..self.pos,
            });
        }
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek_nth(&self, n: usize) -> Option<char> {
        self.src[self.pos..].chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn error(&self, offset: usize, msg: &str) -> AtlantisError {
        let (line, col) = line_col(self.src, offset);
        AtlantisError::Parse(format!("{line}:{col}: {msg}"))
    }

    /// Skips whitespace, `// line` and `/* block */` comments.
    fn skip_trivia(&mut self) -> Result<(), AtlantisError> {
        loop {
            match (self.peek(), self.peek_nth(1)) {
                (Some(c), _) if c.is_whitespace() => {
                    self.bump();
                }
                (Some('/'), Some('/')) => {
                    while let Some(c) = self.bump() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                (Some('/'), Some('*')) => {
                    let start = self.pos;
                    match self.src[self.pos + 2..].find("*/") {
                        Some(end) => self.pos += 2 + end + 2,
                        None => return Err(self.error(start, "unterminated block comment")),
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn token(&mut self, c: char, start: usize) -> Result<Token, AtlantisError> {
        if c.is_ascii_digit() || (c == '.' && self.peek_nth(1).is_some_and(|d| d.is_ascii_digit()))
        {
            return self.number(start);
        }
        if is_ident_start(c) {
            return Ok(Token::Ident(self.word()));
        }
        self.bump();
        let next = self.peek();
        let two = |lexer: &mut Self, tok: Token| {
            lexer.bump();
            Ok(tok)
        };
        match (c, next) {
            ('\'' | '"', _) => self.string(c, start),
            ('`', _) => self.quoted_ident(start).map(Token::QuotedIdent),
            ('$', _) => self.param(start),
            ('.', Some('.')) => two(self, Token::DotDot),
            ('+', Some('=')) => two(self, Token::PlusEq),
            ('=', Some('~')) => two(self, Token::RegexMatch),
            ('<', Some('>')) | ('!', Some('=')) => two(self, Token::NotEq),
            ('<', Some('=')) => two(self, Token::LtEq),
            ('>', Some('=')) => two(self, Token::GtEq),
            ('(', _) => Ok(Token::LParen),
            (')', _) => Ok(Token::RParen),
            ('[', _) => Ok(Token::LBracket),
            (']', _) => Ok(Token::RBracket),
            ('{', _) => Ok(Token::LBrace),
            ('}', _) => Ok(Token::RBrace),
            (',', _) => Ok(Token::Comma),
            ('.', _) => Ok(Token::Dot),
            (':', _) => Ok(Token::Colon),
            (';', _) => Ok(Token::Semicolon),
            ('|', _) => Ok(Token::Pipe),
            ('+', _) => Ok(Token::Plus),
            ('-', _) => Ok(Token::Minus),
            ('*', _) => Ok(Token::Star),
            ('/', _) => Ok(Token::Slash),
            ('%', _) => Ok(Token::Percent),
            ('^', _) => Ok(Token::Caret),
            ('=', _) => Ok(Token::Eq),
            ('<', _) => Ok(Token::Lt),
            ('>', _) => Ok(Token::Gt),
            _ => Err(self.error(start, &format!("unexpected character {c:?}"))),
        }
    }

    fn word(&mut self) -> String {
        let start = self.pos;
        while self.peek().is_some_and(is_ident_continue) {
            self.bump();
        }
        self.src[start..self.pos].to_string()
    }

    fn eat_digits(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
        }
    }

    /// Integer or float. A `.` only continues the number when a digit
    /// follows, so `1..3` lexes as `1`, `..`, `3`.
    fn number(&mut self, start: usize) -> Result<Token, AtlantisError> {
        self.eat_digits();
        let mut is_float = false;
        if self.peek() == Some('.') && self.peek_nth(1).is_some_and(|c| c.is_ascii_digit()) {
            is_float = true;
            self.bump();
            self.eat_digits();
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let digit_at = if matches!(self.peek_nth(1), Some('+' | '-')) {
                2
            } else {
                1
            };
            if self.peek_nth(digit_at).is_some_and(|c| c.is_ascii_digit()) {
                is_float = true;
                for _ in 0..digit_at {
                    self.bump();
                }
                self.eat_digits();
            }
        }
        if self.peek().is_some_and(is_ident_continue) {
            return Err(self.error(start, "invalid number literal"));
        }
        let text = &self.src[start..self.pos];
        if is_float {
            return text
                .parse::<f64>()
                .ok()
                .filter(|x| x.is_finite())
                .map(Token::Float)
                .ok_or_else(|| self.error(start, &format!("float literal out of range: {text}")));
        }
        match text.parse::<u64>() {
            Ok(n) if n <= MAX_INT_MAGNITUDE => Ok(Token::Integer(n)),
            _ => Err(self.error(start, &format!("integer literal out of range: {text}"))),
        }
    }

    /// String body after the opening quote.
    fn string(&mut self, quote: char, start: usize) -> Result<Token, AtlantisError> {
        let mut out = String::new();
        loop {
            let Some(c) = self.bump() else {
                return Err(self.error(start, "unterminated string literal"));
            };
            match c {
                c if c == quote => return Ok(Token::Str(out)),
                '\\' => {
                    let esc_at = self.pos - 1;
                    let Some(e) = self.bump() else {
                        return Err(self.error(start, "unterminated string literal"));
                    };
                    out.push(match e {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        'b' => '\u{8}',
                        'f' => '\u{c}',
                        '0' => '\0',
                        '\\' | '\'' | '"' => e,
                        'u' => self.unicode_escape(esc_at)?,
                        _ => {
                            return Err(
                                self.error(esc_at, &format!("invalid escape sequence \\{e}"))
                            )
                        }
                    });
                }
                c => out.push(c),
            }
        }
    }

    /// The four hex digits of `\uXXXX`.
    fn unicode_escape(&mut self, esc_at: usize) -> Result<char, AtlantisError> {
        let hex = self
            .src
            .get(self.pos..self.pos + 4)
            .filter(|h| h.chars().all(|c| c.is_ascii_hexdigit()));
        let ch = hex
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .and_then(char::from_u32);
        match ch {
            Some(ch) => {
                self.pos += 4;
                Ok(ch)
            }
            None => Err(self.error(esc_at, "invalid unicode escape; expected \\uXXXX")),
        }
    }

    /// Identifier body after the opening backtick; `` `` `` escapes a backtick.
    fn quoted_ident(&mut self, start: usize) -> Result<String, AtlantisError> {
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(self.error(start, "unterminated quoted identifier")),
                Some('`') if self.peek() == Some('`') => {
                    self.bump();
                    out.push('`');
                }
                Some('`') if out.is_empty() => {
                    return Err(self.error(start, "empty quoted identifier"))
                }
                Some('`') => return Ok(out),
                Some(c) => out.push(c),
            }
        }
    }

    fn param(&mut self, start: usize) -> Result<Token, AtlantisError> {
        match self.peek() {
            Some('`') => {
                self.bump();
                self.quoted_ident(start + 1).map(Token::Param)
            }
            Some(c) if is_ident_continue(c) => Ok(Token::Param(self.word())),
            _ => Err(self.error(start, "expected a parameter name after '$'")),
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn toks(src: &str) -> Vec<Token> {
        tokenize(src)
            .unwrap()
            .into_iter()
            .map(|t| t.token)
            .collect()
    }

    fn err(src: &str) -> String {
        tokenize(src).unwrap_err().to_string()
    }

    fn id(s: &str) -> Token {
        Token::Ident(s.into())
    }

    #[test]
    fn arrows_tokenize() {
        use Token::*;
        assert_eq!(
            toks("(a)-->(b)<--(c)--(d)"),
            vec![
                LParen,
                id("a"),
                RParen,
                Minus,
                Minus,
                Gt,
                LParen,
                id("b"),
                RParen,
                Lt,
                Minus,
                Minus,
                LParen,
                id("c"),
                RParen,
                Minus,
                Minus,
                LParen,
                id("d"),
                RParen,
                Eof
            ]
        );
        assert_eq!(
            toks("-[r]->"),
            vec![Minus, LBracket, id("r"), RBracket, Minus, Gt, Eof]
        );
        assert_eq!(
            toks("<-[]-"),
            vec![Lt, Minus, LBracket, RBracket, Minus, Eof]
        );
    }

    #[test]
    fn operators() {
        use Token::*;
        assert_eq!(
            toks("= =~ <> != < <= > >= + += - * / % ^ . .. : ; | , ( ) [ ] { }"),
            vec![
                Eq, RegexMatch, NotEq, NotEq, Lt, LtEq, Gt, GtEq, Plus, PlusEq, Minus, Star, Slash,
                Percent, Caret, Dot, DotDot, Colon, Semicolon, Pipe, Comma, LParen, RParen,
                LBracket, RBracket, LBrace, RBrace, Eof
            ]
        );
    }

    #[test]
    fn numbers() {
        use Token::*;
        assert_eq!(
            toks("42 3.5 1e3 2.5E-2 .5 7e+1"),
            vec![
                Integer(42),
                Float(3.5),
                Float(1000.0),
                Float(0.025),
                Float(0.5),
                Float(70.0),
                Eof
            ]
        );
        assert_eq!(
            toks("*1..3"),
            vec![Star, Integer(1), DotDot, Integer(3), Eof]
        );
        assert_eq!(toks("*..3"), vec![Star, DotDot, Integer(3), Eof]);
        assert_eq!(
            toks("[2..]"),
            vec![LBracket, Integer(2), DotDot, RBracket, Eof]
        );
        assert_eq!(toks("9223372036854775808"), vec![Integer(1 << 63), Eof]);
    }

    #[test]
    fn number_errors() {
        assert!(err("9223372036854775809").contains("integer literal out of range"));
        assert!(err("99999999999999999999999").contains("out of range"));
        assert!(err("1e999").contains("float literal out of range"));
        assert!(err("12abc").contains("invalid number literal"));
    }

    #[test]
    fn strings_and_escapes() {
        assert_eq!(
            toks(r#"'a\nb' "q\"x" 'it\'s' '\\' 'é\t'"#),
            vec![
                Token::Str("a\nb".into()),
                Token::Str("q\"x".into()),
                Token::Str("it's".into()),
                Token::Str("\\".into()),
                Token::Str("\u{e9}\t".into()),
                Token::Eof
            ]
        );
        assert_eq!(
            toks(r#"'say "hi"'"#),
            vec![Token::Str("say \"hi\"".into()), Token::Eof]
        );
    }

    #[test]
    fn string_errors() {
        let e = err("RETURN 'abc");
        assert!(
            e.contains("1:8") && e.contains("unterminated string"),
            "{e}"
        );
        assert!(err(r"'\q'").contains("invalid escape"));
        assert!(err(r"'\u12'").contains("invalid unicode escape"));
        assert!(err(r"'\uD800'").contains("invalid unicode escape"));
    }

    #[test]
    fn identifiers_params_and_comments() {
        use Token::*;
        assert_eq!(
            toks("MATCH `weird name` $p $`odd p` $0 // c\n n /* block\n */ x_1"),
            vec![
                id("MATCH"),
                QuotedIdent("weird name".into()),
                Param("p".into()),
                Param("odd p".into()),
                Param("0".into()),
                id("n"),
                id("x_1"),
                Eof
            ]
        );
        assert_eq!(toks("`a``b`"), vec![QuotedIdent("a`b".into()), Eof]);
    }

    #[test]
    fn lexical_errors_report_position() {
        let e = err("MATCH (n)\nRETURN n # x");
        assert!(e.contains("2:10") && e.contains("'#'"), "{e}");
        assert!(err("/* open").contains("unterminated block comment"));
        assert!(err("`open").contains("unterminated quoted identifier"));
        assert!(err("$ x").contains("parameter name"));
        assert!(err("a ! b").contains("'!'"));
    }

    #[test]
    fn spans_cover_source() {
        let t = tokenize("RETURN  n.name").unwrap();
        assert_eq!(t[0].span, 0..6);
        assert_eq!(t[1].span, 8..9);
        assert_eq!(t[3].span, 10..14);
        assert_eq!(t[4].span, 14..14);
    }

    proptest! {
        #[test]
        fn integer_round_trip(n in 0u64..=(1u64 << 63)) {
            prop_assert_eq!(toks(&n.to_string()), vec![Token::Integer(n), Token::Eof]);
        }

        #[test]
        fn string_round_trip(s in any::<String>()) {
            let mut quoted = String::from("'");
            for c in s.chars() {
                match c {
                    '\'' => quoted.push_str("\\'"),
                    '\\' => quoted.push_str("\\\\"),
                    '\n' => quoted.push_str("\\n"),
                    c => quoted.push(c),
                }
            }
            quoted.push('\'');
            prop_assert_eq!(toks(&quoted), vec![Token::Str(s), Token::Eof]);
        }
    }
}
