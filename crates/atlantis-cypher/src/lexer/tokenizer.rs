//! Cypher tokenizer.
//!
//! Converts a Cypher query string into a stream of spanned tokens.

use atlantis_core::AtlantisError;

use super::token::{SpannedToken, Token};

/// The set of Cypher keywords (case-insensitive).
const KEYWORDS: &[&str] = &[
    "MATCH", "OPTIONAL", "WHERE", "RETURN", "CREATE", "MERGE", "DELETE", "DETACH", "SET", "REMOVE",
    "WITH", "ORDER", "BY", "LIMIT", "SKIP", "AND", "OR", "NOT", "XOR", "CALL", "YIELD", "DISTINCT",
    "AS", "ON", "UNION", "ALL", "UNWIND", "IN", "IS", "NULL", "TRUE", "FALSE", "STARTS", "ENDS",
    "CONTAINS",
];

/// Tokenizes a Cypher query string.
pub fn tokenize(input: &str) -> Result<Vec<SpannedToken>, AtlantisError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut pos = 0;

    while pos < chars.len() {
        // Skip whitespace
        if chars[pos].is_whitespace() {
            pos += 1;
            continue;
        }

        // Skip line comments (//)
        if pos + 1 < chars.len() && chars[pos] == '/' && chars[pos + 1] == '/' {
            while pos < chars.len() && chars[pos] != '\n' {
                pos += 1;
            }
            continue;
        }

        // Skip block comments (/* */)
        if pos + 1 < chars.len() && chars[pos] == '/' && chars[pos + 1] == '*' {
            pos += 2;
            while pos + 1 < chars.len() && !(chars[pos] == '*' && chars[pos + 1] == '/') {
                pos += 1;
            }
            pos += 2;
            continue;
        }

        let start = pos;

        // Strings (single or double quotes)
        if chars[pos] == '\'' || chars[pos] == '"' {
            let quote = chars[pos];
            pos += 1;
            let mut s = String::new();
            while pos < chars.len() && chars[pos] != quote {
                if chars[pos] == '\\' && pos + 1 < chars.len() {
                    pos += 1;
                    match chars[pos] {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        '\\' => s.push('\\'),
                        '\'' => s.push('\''),
                        '"' => s.push('"'),
                        c => s.push(c),
                    }
                    pos += 1;
                } else {
                    s.push(chars[pos]);
                    pos += 1;
                }
            }
            if pos >= chars.len() {
                return Err(AtlantisError::Parse("unterminated string literal".into()));
            }
            pos += 1; // consume closing quote
            let byte_start = char_span_start(input, start);
            let byte_end = char_span_start(input, pos);
            tokens.push(SpannedToken {
                token: Token::String(s),
                span: byte_start..byte_end,
            });
            continue;
        }

        // Numbers (integer or float)
        if chars[pos].is_ascii_digit()
            || (chars[pos] == '-' && pos + 1 < chars.len() && chars[pos + 1].is_ascii_digit())
        {
            let mut num_str = String::new();
            if chars[pos] == '-' {
                num_str.push('-');
                pos += 1;
            }
            let mut is_float = false;
            while pos < chars.len() && (chars[pos].is_ascii_digit() || chars[pos] == '.') {
                if chars[pos] == '.' {
                    is_float = true;
                }
                num_str.push(chars[pos]);
                pos += 1;
            }
            let byte_start = char_span_start(input, start);
            let byte_end = char_span_start(input, pos);
            if is_float {
                let f: f64 = num_str
                    .parse()
                    .map_err(|_| AtlantisError::Parse(format!("invalid float: {num_str}")))?;
                tokens.push(SpannedToken {
                    token: Token::Float(f),
                    span: byte_start..byte_end,
                });
            } else {
                let i: i64 = num_str
                    .parse()
                    .map_err(|_| AtlantisError::Parse(format!("invalid integer: {num_str}")))?;
                tokens.push(SpannedToken {
                    token: Token::Integer(i),
                    span: byte_start..byte_end,
                });
            }
            continue;
        }

        // Parameters ($name)
        if chars[pos] == '$' {
            pos += 1;
            let mut name = String::new();
            while pos < chars.len() && (chars[pos].is_alphanumeric() || chars[pos] == '_') {
                name.push(chars[pos]);
                pos += 1;
            }
            let byte_start = char_span_start(input, start);
            let byte_end = char_span_start(input, pos);
            tokens.push(SpannedToken {
                token: Token::Parameter(name),
                span: byte_start..byte_end,
            });
            continue;
        }

        // Identifiers and keywords
        if chars[pos].is_alphabetic() || chars[pos] == '_' {
            let mut ident = String::new();
            while pos < chars.len() && (chars[pos].is_alphanumeric() || chars[pos] == '_') {
                ident.push(chars[pos]);
                pos += 1;
            }
            let byte_start = char_span_start(input, start);
            let byte_end = char_span_start(input, pos);

            let upper = ident.to_uppercase();
            if KEYWORDS.contains(&upper.as_str()) {
                tokens.push(SpannedToken {
                    token: keyword_from_str(&upper),
                    span: byte_start..byte_end,
                });
            } else {
                tokens.push(SpannedToken {
                    token: Token::Identifier(ident),
                    span: byte_start..byte_end,
                });
            }
            continue;
        }

        // Punctuation
        let token = tokenize_punctuation(&chars, &mut pos, input)?;
        if let Some(tok) = token {
            let byte_start = char_span_start(input, start);
            let byte_end = char_span_start(input, pos);
            tokens.push(SpannedToken {
                token: tok,
                span: byte_start..byte_end,
            });
        }
    }

    tokens.push(SpannedToken {
        token: Token::Eof,
        span: input.len()..input.len(),
    });

    Ok(tokens)
}

fn tokenize_punctuation(
    chars: &[char],
    pos: &mut usize,
    input: &str,
) -> Result<Option<Token>, AtlantisError> {
    let c = chars[*pos];
    *pos += 1;

    let token = match c {
        '(' => Token::LParen,
        ')' => Token::RParen,
        '{' => Token::LBrace,
        '}' => Token::RBrace,
        '[' => Token::LBracket,
        ']' => Token::RBracket,
        ':' => Token::Colon,
        ',' => Token::Comma,
        '.' => Token::Dot,
        '|' => Token::Pipe,
        '=' => Token::Equals,
        '+' => Token::Plus,
        '*' => Token::Star,
        '/' => Token::Slash,
        '%' => Token::Percent,
        '<' => {
            if *pos < chars.len() && chars[*pos] == '=' {
                *pos += 1;
                Token::LtEq
            } else if *pos + 1 < chars.len() && chars[*pos] == '-' && chars[*pos + 1] == '>' {
                *pos += 2;
                Token::ArrowBoth
            } else {
                Token::Lt
            }
        }
        '>' => {
            if *pos < chars.len() && chars[*pos] == '=' {
                *pos += 1;
                Token::GtEq
            } else {
                Token::Gt
            }
        }
        '-' => {
            if *pos < chars.len() && chars[*pos] == '>' {
                *pos += 1;
                Token::ArrowRight
            } else {
                Token::Minus
            }
        }
        '!' => {
            if *pos < chars.len() && chars[*pos] == '=' {
                *pos += 1;
                Token::NotEquals
            } else {
                return Err(AtlantisError::Parse(format!("unexpected character: '!' at position {pos}")));
            }
        }
        _ => {
            return Err(AtlantisError::Parse(format!(
                "unexpected character: '{c}' at position {}",
                *pos - 1
            )))
        }
    };

    let _ = input;
    Ok(Some(token))
}

fn keyword_from_str(s: &str) -> Token {
    match s {
        "MATCH" => Token::Match,
        "OPTIONAL" => Token::Optional,
        "WHERE" => Token::Where,
        "RETURN" => Token::Return,
        "CREATE" => Token::Create,
        "MERGE" => Token::Merge,
        "DELETE" => Token::Delete,
        "DETACH" => Token::Detach,
        "SET" => Token::Set,
        "REMOVE" => Token::Remove,
        "WITH" => Token::With,
        "ORDER" => Token::Order,
        "BY" => Token::By,
        "LIMIT" => Token::Limit,
        "SKIP" => Token::Skip,
        "AND" => Token::And,
        "OR" => Token::Or,
        "NOT" => Token::Not,
        "XOR" => Token::Xor,
        "CALL" => Token::Call,
        "YIELD" => Token::Yield,
        "DISTINCT" => Token::Distinct,
        "AS" => Token::As,
        "ON" => Token::On,
        "UNION" => Token::Union,
        "ALL" => Token::All,
        "UNWIND" => Token::Unwind,
        "IN" => Token::In,
        "IS" => Token::Is,
        "NULL" => Token::Null,
        "TRUE" => Token::True,
        "FALSE" => Token::False,
        "STARTS" => Token::Starts,
        "ENDS" => Token::Ends,
        "CONTAINS" => Token::Contains,
        _ => Token::Identifier(s.to_lowercase()),
    }
}

/// Converts a character index to a byte offset in the original string.
fn char_span_start(input: &str, char_index: usize) -> usize {
    input.char_indices().nth(char_index).map(|(b, _)| b).unwrap_or(input.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_types(tokens: &[SpannedToken]) -> Vec<&Token> {
        tokens.iter().map(|st| &st.token).collect()
    }

    #[test]
    fn test_tokenize_simple_match() {
        let tokens = tokenize("MATCH (n:Person) RETURN n").unwrap();
        assert!(token_types(&tokens).contains(&&Token::Match));
        assert!(token_types(&tokens).contains(&&Token::Return));
        assert!(token_types(&tokens).contains(&&Token::Identifier("n".into())));
    }

    #[test]
    fn test_tokenize_keywords_case_insensitive() {
        let tokens = tokenize("match RETURN where").unwrap();
        assert!(token_types(&tokens).contains(&&Token::Match));
        assert!(token_types(&tokens).contains(&&Token::Return));
        assert!(token_types(&tokens).contains(&&Token::Where));
    }

    #[test]
    fn test_tokenize_numbers() {
        let tokens = tokenize("42 3.14 -5").unwrap();
        assert!(token_types(&tokens).contains(&&Token::Integer(42)));
        assert!(token_types(&tokens).contains(&&Token::Float(3.14)));
    }

    #[test]
    fn test_tokenize_strings() {
        let tokens = tokenize("'hello' \"world\"").unwrap();
        assert!(token_types(&tokens).contains(&&Token::String("hello".into())));
        assert!(token_types(&tokens).contains(&&Token::String("world".into())));
    }

    #[test]
    fn test_tokenize_parameters() {
        let tokens = tokenize("$name $age").unwrap();
        assert!(token_types(&tokens).contains(&&Token::Parameter("name".into())));
        assert!(token_types(&tokens).contains(&&Token::Parameter("age".into())));
    }

    #[test]
    fn test_tokenize_arrows() {
        let tokens = tokenize("--> <-- <-->").unwrap();
        assert!(token_types(&tokens).contains(&&Token::ArrowRight));
        assert!(token_types(&tokens).contains(&&Token::ArrowLeft));
        assert!(token_types(&tokens).contains(&&Token::ArrowBoth));
    }

    #[test]
    fn test_tokenize_comments() {
        let tokens = tokenize("// this is a comment\nMATCH (n)").unwrap();
        assert!(token_types(&tokens).contains(&&Token::Match));
    }

    #[test]
    fn test_tokenize_block_comments() {
        let tokens = tokenize("/* block comment */MATCH (n)").unwrap();
        assert!(token_types(&tokens).contains(&&Token::Match));
    }

    #[test]
    fn test_unterminated_string() {
        let result = tokenize("'unterminated");
        assert!(matches!(result, Err(AtlantisError::Parse(_))));
    }

    #[test]
    fn test_tokenize_punctuation() {
        let tokens = tokenize("(){}[]:,.=<>!=+-*/%").unwrap();
        assert!(token_types(&tokens).contains(&&Token::LParen));
        assert!(token_types(&tokens).contains(&&Token::RParen));
        assert!(token_types(&tokens).contains(&&Token::LBrace));
        assert!(token_types(&tokens).contains(&&Token::RBrace));
    }

    #[test]
    fn test_tokenize_full_query() {
        let query = "MATCH (a:Person)-[:KNOWS]->(b:Person) WHERE a.name = 'Alice' RETURN b.name";
        let tokens = tokenize(query).unwrap();
        assert!(tokens.len() > 10);
        assert_eq!(tokens.last().unwrap().token, Token::Eof);
    }
}
