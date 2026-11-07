//! Expressions, by precedence (lowest first): `OR`, `XOR`, `AND`, `NOT`,
//! comparison, `+ -`, `* / %`, `^`, unary `- +`, postfix, atoms.

use atlantis_core::Value;

use super::ast::{BinaryOp, Expression, UnaryOp};
use super::cursor::{PResult, Parser};
use crate::lexer::Token;

/// Words that cannot be a bare variable in an expression because they
/// start or continue another construct. Backticks lift the restriction.
const RESERVED: &[&str] = &[
    "MATCH", "OPTIONAL", "WHERE", "RETURN", "CREATE", "MERGE", "SET", "REMOVE", "DELETE", "DETACH",
    "WITH", "UNWIND", "CALL", "YIELD", "UNION", "AND", "OR", "XOR", "NOT", "IN", "IS", "AS",
    "STARTS", "ENDS", "CONTAINS", "WHEN", "THEN", "ELSE", "END",
];

fn binary(lhs: Expression, op: BinaryOp, rhs: Expression) -> Expression {
    Expression::Binary(Box::new(lhs), op, Box::new(rhs))
}

impl Parser<'_> {
    /// A full expression.
    pub(crate) fn expression(&mut self) -> PResult<Expression> {
        self.or_expr()
    }

    /// Left-associative chain of keyword operator `kw` over `next`.
    fn keyword_chain(
        &mut self,
        kw: &str,
        op: BinaryOp,
        next: fn(&mut Self) -> PResult<Expression>,
    ) -> PResult<Expression> {
        let mut lhs = next(self)?;
        while self.eat_kw(kw) {
            let rhs = next(self)?;
            lhs = binary(lhs, op, rhs);
        }
        Ok(lhs)
    }

    fn or_expr(&mut self) -> PResult<Expression> {
        self.keyword_chain("OR", BinaryOp::Or, Self::xor_expr)
    }

    fn xor_expr(&mut self) -> PResult<Expression> {
        self.keyword_chain("XOR", BinaryOp::Xor, Self::and_expr)
    }

    fn and_expr(&mut self) -> PResult<Expression> {
        self.keyword_chain("AND", BinaryOp::And, Self::not_expr)
    }

    fn not_expr(&mut self) -> PResult<Expression> {
        if self.eat_kw("NOT") {
            let operand = self.not_expr()?;
            return Ok(Expression::Unary(UnaryOp::Not, Box::new(operand)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> PResult<Expression> {
        let mut lhs = self.additive()?;
        loop {
            let op = match self.peek() {
                Token::Eq => BinaryOp::Eq,
                Token::NotEq => BinaryOp::NotEq,
                Token::Lt => BinaryOp::Lt,
                Token::LtEq => BinaryOp::LtEq,
                Token::Gt => BinaryOp::Gt,
                Token::GtEq => BinaryOp::GtEq,
                Token::RegexMatch => BinaryOp::RegexMatch,
                Token::Colon => {
                    let labels = self.label_list()?;
                    lhs = Expression::HasLabels(Box::new(lhs), labels);
                    continue;
                }
                t if t.is_keyword("IN") => BinaryOp::In,
                t if t.is_keyword("CONTAINS") => BinaryOp::Contains,
                t if t.is_keyword("STARTS") => {
                    self.bump();
                    self.expect_kw("WITH")?;
                    let rhs = self.additive()?;
                    lhs = binary(lhs, BinaryOp::StartsWith, rhs);
                    continue;
                }
                t if t.is_keyword("ENDS") => {
                    self.bump();
                    self.expect_kw("WITH")?;
                    let rhs = self.additive()?;
                    lhs = binary(lhs, BinaryOp::EndsWith, rhs);
                    continue;
                }
                t if t.is_keyword("IS") => {
                    self.bump();
                    let negated = self.eat_kw("NOT");
                    self.expect_kw("NULL")?;
                    lhs = Expression::IsNull(Box::new(lhs), negated);
                    continue;
                }
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.additive()?;
            lhs = binary(lhs, op, rhs);
        }
    }

    /// `:A:B` — one or more labels, each introduced by a colon.
    pub(crate) fn label_list(&mut self) -> PResult<Vec<String>> {
        let mut labels = Vec::new();
        while self.eat(&Token::Colon) {
            labels.push(self.name("a label")?);
        }
        if labels.is_empty() {
            return Err(self.unexpected("':'"));
        }
        Ok(labels)
    }

    fn additive(&mut self) -> PResult<Expression> {
        let mut lhs = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinaryOp::Add,
                Token::Minus => BinaryOp::Sub,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.multiplicative()?;
            lhs = binary(lhs, op, rhs);
        }
    }

    fn multiplicative(&mut self) -> PResult<Expression> {
        let mut lhs = self.power()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinaryOp::Mul,
                Token::Slash => BinaryOp::Div,
                Token::Percent => BinaryOp::Mod,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.power()?;
            lhs = binary(lhs, op, rhs);
        }
    }

    /// `^` is left-associative, as in openCypher.
    fn power(&mut self) -> PResult<Expression> {
        let mut lhs = self.unary()?;
        while self.eat(&Token::Caret) {
            let rhs = self.unary()?;
            lhs = binary(lhs, BinaryOp::Pow, rhs);
        }
        Ok(lhs)
    }

    /// Unary `-`/`+`. Negated numeric literals fold into a literal so that
    /// `-9223372036854775808` yields `i64::MIN`.
    fn unary(&mut self) -> PResult<Expression> {
        if self.eat(&Token::Plus) {
            let operand = self.unary()?;
            return Ok(Expression::Unary(UnaryOp::Plus, Box::new(operand)));
        }
        if !self.eat(&Token::Minus) {
            return self.postfix();
        }
        if self.peek() == &Token::Integer(i64::MIN.unsigned_abs())
            && !matches!(self.peek_nth(1), Token::Dot | Token::LBracket)
        {
            self.bump();
            return Ok(Expression::Literal(Value::Integer(i64::MIN)));
        }
        Ok(match self.unary()? {
            Expression::Literal(Value::Integer(n)) if n != i64::MIN => {
                Expression::Literal(Value::Integer(-n))
            }
            Expression::Literal(Value::Float(x)) => Expression::Literal(Value::Float(-x)),
            operand => Expression::Unary(UnaryOp::Neg, Box::new(operand)),
        })
    }

    /// Property access `.key`, index `[i]` and slice `[a..b]`.
    fn postfix(&mut self) -> PResult<Expression> {
        let mut expr = self.atom()?;
        loop {
            if self.eat(&Token::Dot) {
                let key = self.name("a property key")?;
                expr = Expression::Property(Box::new(expr), key);
            } else if self.eat(&Token::LBracket) {
                expr = self.index_or_slice(expr)?;
            } else {
                return Ok(expr);
            }
        }
    }

    /// After `[`: `i]`, `a..b]`, `..b]`, `a..]` or `..]`.
    fn index_or_slice(&mut self, target: Expression) -> PResult<Expression> {
        if self.eat(&Token::DotDot) {
            return self.slice_end(target, None);
        }
        let from = self.expression()?;
        if self.eat(&Token::DotDot) {
            return self.slice_end(target, Some(from));
        }
        self.expect(&Token::RBracket, "'..' or ']'")?;
        Ok(Expression::Index(Box::new(target), Box::new(from)))
    }

    /// The optional upper bound and `]` of a slice, after `..`.
    fn slice_end(&mut self, target: Expression, from: Option<Expression>) -> PResult<Expression> {
        let to = if self.at(&Token::RBracket) {
            None
        } else {
            Some(Box::new(self.expression()?))
        };
        self.expect(&Token::RBracket, "']'")?;
        Ok(Expression::Slice(Box::new(target), from.map(Box::new), to))
    }

    fn atom(&mut self) -> PResult<Expression> {
        let offset = self.offset();
        match self.peek().clone() {
            Token::Integer(n) => {
                self.bump();
                i64::try_from(n)
                    .map(|n| Expression::Literal(Value::Integer(n)))
                    .map_err(|_| {
                        self.error_at(offset, &format!("integer literal out of range: {n}"))
                    })
            }
            Token::Float(x) => {
                self.bump();
                Ok(Expression::Literal(Value::Float(x)))
            }
            Token::Str(s) => {
                self.bump();
                Ok(Expression::Literal(Value::String(s)))
            }
            Token::Param(p) => {
                self.bump();
                Ok(Expression::Parameter(p))
            }
            Token::LParen => {
                self.bump();
                let inner = self.expression()?;
                self.expect(&Token::RParen, "')'")?;
                Ok(inner)
            }
            Token::LBracket => self.list_or_comprehension(),
            Token::LBrace => Ok(Expression::Map(self.map_literal()?)),
            Token::Ident(_) | Token::QuotedIdent(_) => self.named_atom(),
            _ => Err(self.unexpected("an expression")),
        }
    }

    /// Literal keywords, `CASE`, function calls and variables.
    fn named_atom(&mut self) -> PResult<Expression> {
        let tok = self.peek().clone();
        if tok.is_keyword("true") || tok.is_keyword("false") || tok.is_keyword("null") {
            self.bump();
            let value = match tok {
                t if t.is_keyword("null") => Value::Null,
                t => Value::Bool(t.is_keyword("true")),
            };
            return Ok(Expression::Literal(value));
        }
        if tok.is_keyword("CASE") {
            return self.case_expr();
        }
        if let Some(parts) = self.function_name_parts() {
            return self.function_call(parts);
        }
        if let Token::Ident(word) = &tok {
            if RESERVED.iter().any(|kw| word.eq_ignore_ascii_case(kw)) {
                return Err(self.unexpected("an expression"));
            }
        }
        Ok(Expression::Variable(self.name("a variable")?))
    }

    /// Number of name tokens in `a.b.c(` if the cursor starts a function
    /// call, without consuming anything.
    fn function_name_parts(&self) -> Option<usize> {
        let mut i = 0;
        loop {
            if !self.is_name_at(i) {
                return None;
            }
            match self.peek_nth(i + 1) {
                Token::LParen => return Some(i / 2 + 1),
                Token::Dot => i += 2,
                _ => return None,
            }
        }
    }

    /// Dotted name of `parts` segments; used for functions and procedures.
    pub(crate) fn dotted_name(&mut self, parts: usize) -> PResult<String> {
        let mut segments = Vec::with_capacity(parts);
        for i in 0..parts {
            if i > 0 {
                self.expect(&Token::Dot, "'.'")?;
            }
            segments.push(self.name("a name")?);
        }
        Ok(segments.join("."))
    }

    fn function_call(&mut self, parts: usize) -> PResult<Expression> {
        let name = self.dotted_name(parts)?;
        self.expect(&Token::LParen, "'('")?;
        if name.eq_ignore_ascii_case("count")
            && self.at(&Token::Star)
            && self.peek_nth(1) == &Token::RParen
        {
            self.bump();
            self.bump();
            return Ok(Expression::CountStar);
        }
        let distinct = self.eat_kw("DISTINCT");
        let args = self.call_args()?;
        Ok(Expression::Function {
            name,
            distinct,
            args,
        })
    }

    /// Comma-separated expressions up to and including `)`.
    pub(crate) fn call_args(&mut self) -> PResult<Vec<Expression>> {
        let mut args = Vec::new();
        if self.eat(&Token::RParen) {
            return Ok(args);
        }
        loop {
            args.push(self.expression()?);
            if !self.eat(&Token::Comma) {
                self.expect(&Token::RParen, "',' or ')'")?;
                return Ok(args);
            }
        }
    }

    fn case_expr(&mut self) -> PResult<Expression> {
        self.expect_kw("CASE")?;
        if self.at_kw("ELSE") || self.at_kw("END") {
            return Err(self.unexpected("WHEN"));
        }
        let operand = if self.at_kw("WHEN") {
            None
        } else {
            Some(Box::new(self.expression()?))
        };
        let mut whens = Vec::new();
        while self.eat_kw("WHEN") {
            let cond = self.expression()?;
            self.expect_kw("THEN")?;
            whens.push((cond, self.expression()?));
        }
        if whens.is_empty() {
            return Err(self.unexpected("WHEN"));
        }
        let default = if self.eat_kw("ELSE") {
            Some(Box::new(self.expression()?))
        } else {
            None
        };
        self.expect_kw("END")?;
        Ok(Expression::Case {
            operand,
            whens,
            default,
        })
    }

    /// `[a, b]` or `[x IN list [WHERE p] [| e]]`.
    fn list_or_comprehension(&mut self) -> PResult<Expression> {
        self.expect(&Token::LBracket, "'['")?;
        if self.is_name_at(0) && self.peek_nth(1).is_keyword("IN") {
            let variable = self.name("a variable")?;
            self.expect_kw("IN")?;
            let list = Box::new(self.expression()?);
            let predicate = if self.eat_kw("WHERE") {
                Some(Box::new(self.expression()?))
            } else {
                None
            };
            let map = if self.eat(&Token::Pipe) {
                Some(Box::new(self.expression()?))
            } else {
                None
            };
            self.expect(&Token::RBracket, "']'")?;
            return Ok(Expression::ListComprehension {
                variable,
                list,
                predicate,
                map,
            });
        }
        let mut items = Vec::new();
        if self.eat(&Token::RBracket) {
            return Ok(Expression::List(items));
        }
        loop {
            items.push(self.expression()?);
            if !self.eat(&Token::Comma) {
                self.expect(&Token::RBracket, "',' or ']'")?;
                return Ok(Expression::List(items));
            }
        }
    }

    /// `{key: expr, ...}`; also used for pattern properties and options.
    pub(crate) fn map_literal(&mut self) -> PResult<Vec<(String, Expression)>> {
        self.expect(&Token::LBrace, "'{'")?;
        let mut entries = Vec::new();
        if self.eat(&Token::RBrace) {
            return Ok(entries);
        }
        loop {
            let key = self.name("a map key")?;
            self.expect(&Token::Colon, "':'")?;
            entries.push((key, self.expression()?));
            if !self.eat(&Token::Comma) {
                self.expect(&Token::RBrace, "',' or '}'")?;
                return Ok(entries);
            }
        }
    }
}
