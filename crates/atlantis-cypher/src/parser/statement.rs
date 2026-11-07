//! Top-level Cypher statement parser.
//!
//! Uses chumsky parser combinators to parse a token stream into an AST.

use atlantis_core::AtlantisError;

use crate::lexer::{tokenize, Token};
use crate::lexer::SpannedToken;

use super::ast::*;

/// Parses a Cypher query string into an AST `Statement`.
pub fn parse(input: &str) -> Result<Statement, AtlantisError> {
    let tokens = tokenize(input)?;
    parse_tokens(&tokens)
}

/// Parses a list of spanned tokens into an AST `Statement`.
fn parse_tokens(tokens: &[SpannedToken]) -> Result<Statement, AtlantisError> {
    let mut parser = Parser::new(tokens);
    parser.parse_statement()
}

/// A simple recursive-descent parser over the token stream.
///
/// While we use chumsky for the final implementation, this hand-written
/// parser handles the core Cypher subset for v1.
struct Parser<'a> {
    tokens: &'a [SpannedToken],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [SpannedToken]) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn current(&self) -> &Token {
        &self.tokens[self.pos].token
    }

    fn advance(&mut self) -> &Token {
        let token = &self.tokens[self.pos].token;
        self.pos += 1;
        token
    }

    fn expect(&mut self, expected: &Token) -> Result<(), AtlantisError> {
        if self.current() == expected {
            self.advance();
            Ok(())
        } else {
            Err(AtlantisError::Parse(format!(
                "expected {:?}, got {:?}",
                expected,
                self.current()
            )))
        }
    }

    fn is_eof(&self) -> bool {
        matches!(self.current(), Token::Eof)
    }

    fn parse_statement(&mut self) -> Result<Statement, AtlantisError> {
        let mut clauses = Vec::new();
        let mut result = None;

        while !self.is_eof() {
            match self.current() {
                Token::Match => {
                    let clause = self.parse_match(false)?;
                    clauses.push(Clause::Match(clause));
                }
                Token::Optional => {
                    self.advance(); // consume OPTIONAL
                    let clause = self.parse_match(true)?;
                    clauses.push(Clause::Match(clause));
                }
                Token::Where => {
                    self.advance();
                    let expr = self.parse_expression()?;
                    clauses.push(Clause::Where(expr));
                }
                Token::Create => {
                    self.advance();
                    let pattern = self.parse_pattern()?;
                    clauses.push(Clause::Create(CreateClause { pattern }));
                }
                Token::Return => {
                    self.advance();
                    let return_clause = self.parse_return()?;
                    result = Some(return_clause);
                    break;
                }
                Token::With => {
                    self.advance();
                    let with_clause = self.parse_with()?;
                    clauses.push(Clause::With(with_clause));
                }
                Token::Set => {
                    self.advance();
                    let set_clause = self.parse_set()?;
                    clauses.push(Clause::Set(set_clause));
                }
                Token::Delete => {
                    self.advance();
                    let exprs = self.parse_expression_list()?;
                    clauses.push(Clause::Delete(DeleteClause {
                        detach: false,
                        expressions: exprs,
                    }));
                }
                Token::Detach => {
                    self.advance();
                    self.expect(&Token::Delete)?;
                    let exprs = self.parse_expression_list()?;
                    clauses.push(Clause::Delete(DeleteClause {
                        detach: true,
                        expressions: exprs,
                    }));
                }
                Token::Call => {
                    self.advance();
                    let call_clause = self.parse_call()?;
                    clauses.push(Clause::Call(call_clause));
                }
                Token::Unwind => {
                    self.advance();
                    let unwind_clause = self.parse_unwind()?;
                    clauses.push(Clause::Unwind(unwind_clause));
                }
                Token::Eof => break,
                _ => {
                    return Err(AtlantisError::Parse(format!(
                        "unexpected token: {:?}",
                        self.current()
                    )))
                }
            }
        }

        Ok(Statement::Query(Query { clauses, result }))
    }

    fn parse_match(&mut self, optional: bool) -> Result<MatchClause, AtlantisError> {
        self.expect(&Token::Match)?;
        let pattern = self.parse_pattern()?;
        Ok(MatchClause { pattern, optional })
    }

    fn parse_pattern(&mut self) -> Result<Pattern, AtlantisError> {
        let mut elements = Vec::new();

        // First element must be a node
        let node = self.parse_node_pattern()?;
        elements.push(PatternElement::Node(node));

        // Followed by optional relationship + node pairs
        while matches!(
            self.current(),
            Token::Minus | Token::ArrowLeft | Token::ArrowRight | Token::ArrowBoth
        ) {
            let rel = self.parse_relationship_pattern()?;
            let node = self.parse_node_pattern()?;
            elements.push(PatternElement::Relationship(rel));
            elements.push(PatternElement::Node(node));
        }

        Ok(Pattern { elements })
    }

    fn parse_node_pattern(&mut self) -> Result<NodePattern, AtlantisError> {
        self.expect(&Token::LParen)?;

        let variable = if let Token::Identifier(name) = self.current() {
            let name = name.clone();
            self.advance();
            Some(name)
        } else {
            None
        };

        let mut labels = Vec::new();
        while matches!(self.current(), Token::Colon) {
            self.advance();
            if let Token::Identifier(name) = self.current() {
                labels.push(atlantis_core::Label::from(name.clone()));
                self.advance();
            }
        }

        let properties = self.parse_inline_properties()?;

        self.expect(&Token::RParen)?;
        Ok(NodePattern {
            variable,
            labels,
            properties,
        })
    }

    fn parse_relationship_pattern(&mut self) -> Result<RelationshipPattern, AtlantisError> {
        // Determine direction from arrow tokens
        let direction = match self.current() {
            Token::ArrowLeft => {
                self.advance();
                self.parse_rel_body(atlantis_core::Direction::Incoming)
            }
            Token::ArrowRight => {
                self.advance();
                self.parse_rel_body(atlantis_core::Direction::Outgoing)
            }
            Token::ArrowBoth => {
                self.advance();
                self.parse_rel_body(atlantis_core::Direction::Both)
            }
            _ => {
                // Handle `-[]->` syntax: consume `-`, parse body, expect `->`
                self.expect(&Token::Minus)?;
                let mut pattern = self.parse_rel_body(atlantis_core::Direction::Outgoing)?;
                if matches!(self.current(), Token::ArrowRight) {
                    self.advance();
                }
                Ok(pattern)
            }
        }?;

        Ok(direction)
    }

    fn parse_rel_body(
        &mut self,
        direction: atlantis_core::Direction,
    ) -> Result<RelationshipPattern, AtlantisError> {
        let mut variable = None;
        let mut rel_type = None;
        let mut properties = Vec::new();
        let mut length = None;

        if matches!(self.current(), Token::LBracket) {
            self.advance();

            if let Token::Identifier(name) = self.current() {
                variable = Some(name.clone());
                self.advance();
            }

            if matches!(self.current(), Token::Colon) {
                self.advance();
                if let Token::Identifier(name) = self.current() {
                    rel_type = Some(atlantis_core::RelType::from(name.clone()));
                    self.advance();
                }
            }

            // Variable-length: *min..max
            if matches!(self.current(), Token::Star) {
                self.advance();
                let min = if let Token::Integer(n) = self.current().clone() {
                    self.advance();
                    Some(n as usize)
                } else {
                    None
                };

                let max = if matches!(self.current(), Token::Dot) {
                    self.advance();
                    self.advance(); // second dot
                    if let Token::Integer(n) = self.current().clone() {
                        self.advance();
                        Some(n as usize)
                    } else {
                        None
                    }
                } else {
                    None
                };

                length = Some(PathLength { min, max });
            }

            properties = self.parse_inline_properties()?;
            self.expect(&Token::RBracket)?;
        }

        // If direction was Outgoing from `-`, expect the closing `>`
        if direction == atlantis_core::Direction::Outgoing && matches!(self.current(), Token::Minus) {
            // Already handled
        }

        Ok(RelationshipPattern {
            variable,
            rel_type,
            direction,
            properties,
            length,
        })
    }

    fn parse_inline_properties(&mut self) -> Result<Vec<(String, Expression)>, AtlantisError> {
        let mut props = Vec::new();
        if matches!(self.current(), Token::LBrace) {
            self.advance();
            loop {
                if matches!(self.current(), Token::RBrace) {
                    self.advance();
                    break;
                }

                let key = if let Token::Identifier(name) = self.current() {
                    let name = name.clone();
                    self.advance();
                    name
                } else {
                    return Err(AtlantisError::Parse(format!(
                        "expected property key, got {:?}",
                        self.current()
                    )));
                };

                self.expect(&Token::Colon)?;
                let value = self.parse_expression()?;
                props.push((key, value));

                if matches!(self.current(), Token::Comma) {
                    self.advance();
                }
            }
        }
        Ok(props)
    }

    fn parse_return(&mut self) -> Result<ReturnClause, AtlantisError> {
        let distinct = if matches!(self.current(), Token::Distinct) {
            self.advance();
            true
        } else {
            false
        };

        let items = self.parse_return_items()?;

        let order_by = if matches!(self.current(), Token::Order) {
            self.advance();
            self.expect(&Token::By)?;
            Some(self.parse_order_items()?)
        } else {
            None
        };

        let skip = if matches!(self.current(), Token::Skip) {
            self.advance();
            Some(self.parse_expression()?)
        } else {
            None
        };

        let limit = if matches!(self.current(), Token::Limit) {
            self.advance();
            Some(self.parse_expression()?)
        } else {
            None
        };

        Ok(ReturnClause {
            distinct,
            items,
            order_by,
            skip,
            limit,
        })
    }

    fn parse_return_items(&mut self) -> Result<Vec<ReturnItem>, AtlantisError> {
        let mut items = Vec::new();
        loop {
            if matches!(self.current(), Token::Star) {
                self.advance();
                items.push(ReturnItem {
                    expression: Expression::Variable("*".into()),
                    alias: None,
                });
            } else {
                let expr = self.parse_expression()?;
                let alias = if matches!(self.current(), Token::As) {
                    self.advance();
                    if let Token::Identifier(name) = self.current() {
                        let name = name.clone();
                        self.advance();
                        Some(name)
                    } else {
                        None
                    }
                } else {
                    None
                };
                items.push(ReturnItem {
                    expression: expr,
                    alias,
                });
            }

            if matches!(self.current(), Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        Ok(items)
    }

    fn parse_order_items(&mut self) -> Result<Vec<OrderItem>, AtlantisError> {
        let mut items = Vec::new();
        loop {
            let expr = self.parse_expression()?;
            let descending = if matches!(self.current(), Token::Identifier(name) if name == "DESC" || name == "DESCENDING")
            {
                self.advance();
                true
            } else {
                false
            };
            items.push(OrderItem {
                expression: expr,
                descending,
            });

            if matches!(self.current(), Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        Ok(items)
    }

    fn parse_with(&mut self) -> Result<WithClause, AtlantisError> {
        let distinct = if matches!(self.current(), Token::Distinct) {
            self.advance();
            true
        } else {
            false
        };

        let items = self.parse_return_items()?;

        let order_by = if matches!(self.current(), Token::Order) {
            self.advance();
            self.expect(&Token::By)?;
            Some(self.parse_order_items()?)
        } else {
            None
        };

        let skip = if matches!(self.current(), Token::Skip) {
            self.advance();
            Some(self.parse_expression()?)
        } else {
            None
        };

        let limit = if matches!(self.current(), Token::Limit) {
            self.advance();
            Some(self.parse_expression()?)
        } else {
            None
        };

        let where_clause = if matches!(self.current(), Token::Where) {
            self.advance();
            Some(self.parse_expression()?)
        } else {
            None
        };

        Ok(WithClause {
            distinct,
            items,
            order_by,
            skip,
            limit,
            where_clause,
        })
    }

    fn parse_set(&mut self) -> Result<SetClause, AtlantisError> {
        let mut assignments = Vec::new();
        loop {
            let target = self.parse_expression()?;
            self.expect(&Token::Equals)?;
            let value = self.parse_expression()?;
            assignments.push(SetAssignment { target, value });

            if matches!(self.current(), Token::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        Ok(SetClause { assignments })
    }

    fn parse_call(&mut self) -> Result<CallClause, AtlantisError> {
        // Parse the procedure name (dotted identifier: db.vector.search)
        let mut parts = Vec::new();
        if let Token::Identifier(name) = self.current() {
            parts.push(name.clone());
            self.advance();
        }
        while matches!(self.current(), Token::Dot) {
            self.advance();
            if let Token::Identifier(name) = self.current() {
                parts.push(name.clone());
                self.advance();
            }
        }

        let procedure = parts.join(".");

        self.expect(&Token::LParen)?;
        let arguments = self.parse_expression_list()?;
        self.expect(&Token::RParen)?;

        let yield_items = if matches!(self.current(), Token::Yield) {
            self.advance();
            let mut items = Vec::new();
            loop {
                if let Token::Identifier(name) = self.current() {
                    let name = name.clone();
                    self.advance();
                    let alias = if matches!(self.current(), Token::As) {
                        self.advance();
                        if let Token::Identifier(a) = self.current() {
                            let a = a.clone();
                            self.advance();
                            Some(a)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    items.push((name, alias));
                }

                if matches!(self.current(), Token::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
            Some(items)
        } else {
            None
        };

        Ok(CallClause {
            procedure,
            arguments,
            yield_items,
        })
    }

    fn parse_unwind(&mut self) -> Result<UnwindClause, AtlantisError> {
        let expression = self.parse_expression()?;
        self.expect(&Token::As)?;
        let variable = if let Token::Identifier(name) = self.current() {
            let name = name.clone();
            self.advance();
            name
        } else {
            return Err(AtlantisError::Parse("expected variable after AS in UNWIND".into()));
        };
        Ok(UnwindClause { expression, variable })
    }

    fn parse_expression_list(&mut self) -> Result<Vec<Expression>, AtlantisError> {
        let mut exprs = Vec::new();
        exprs.push(self.parse_expression()?);
        while matches!(self.current(), Token::Comma) {
            self.advance();
            exprs.push(self.parse_expression()?);
        }
        Ok(exprs)
    }

    fn parse_expression(&mut self) -> Result<Expression, AtlantisError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expression, AtlantisError> {
        let mut left = self.parse_and()?;
        while matches!(self.current(), Token::Or) {
            self.advance();
            let right = self.parse_and()?;
            left = Expression::BinaryOp {
                left: Box::new(left),
                op: BinaryOp::Or,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expression, AtlantisError> {
        let mut left = self.parse_comparison()?;
        while matches!(self.current(), Token::And) {
            self.advance();
            let right = self.parse_comparison()?;
            left = Expression::BinaryOp {
                left: Box::new(left),
                op: BinaryOp::And,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expression, AtlantisError> {
        let mut left = self.parse_additive()?;
        loop {
            let op = match self.current() {
                Token::Equals => BinaryOp::Eq,
                Token::NotEquals => BinaryOp::NotEq,
                Token::Lt => BinaryOp::Lt,
                Token::Gt => BinaryOp::Gt,
                Token::LtEq => BinaryOp::LtEq,
                Token::GtEq => BinaryOp::GtEq,
                _ => break,
            };
            self.advance();
            let right = self.parse_additive()?;
            left = Expression::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_additive(&mut self) -> Result<Expression, AtlantisError> {
        let mut left = self.parse_multiplicative()?;
        loop {
            let op = match self.current() {
                Token::Plus => BinaryOp::Add,
                Token::Minus => BinaryOp::Sub,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplicative()?;
            left = Expression::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_multiplicative(&mut self) -> Result<Expression, AtlantisError> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.current() {
                Token::Star => BinaryOp::Mul,
                Token::Slash => BinaryOp::Div,
                Token::Percent => BinaryOp::Mod,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            left = Expression::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expression, AtlantisError> {
        if matches!(self.current(), Token::Minus) {
            self.advance();
            let operand = self.parse_primary()?;
            return Ok(Expression::UnaryOp {
                op: UnaryOp::Neg,
                operand: Box::new(operand),
            });
        }
        if matches!(self.current(), Token::Not) {
            self.advance();
            let operand = self.parse_primary()?;
            return Ok(Expression::UnaryOp {
                op: UnaryOp::Not,
                operand: Box::new(operand),
            });
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expression, AtlantisError> {
        match self.current().clone() {
            Token::Integer(i) => {
                self.advance();
                Ok(Expression::Literal(atlantis_core::Value::Integer(i)))
            }
            Token::Float(f) => {
                self.advance();
                Ok(Expression::Literal(atlantis_core::Value::Float(f)))
            }
            Token::String(s) => {
                self.advance();
                Ok(Expression::Literal(atlantis_core::Value::String(s)))
            }
            Token::True => {
                self.advance();
                Ok(Expression::Literal(atlantis_core::Value::Bool(true)))
            }
            Token::False => {
                self.advance();
                Ok(Expression::Literal(atlantis_core::Value::Bool(false)))
            }
            Token::Null => {
                self.advance();
                Ok(Expression::Literal(atlantis_core::Value::Null))
            }
            Token::Parameter(name) => {
                self.advance();
                Ok(Expression::Parameter(name))
            }
            Token::Identifier(name) => {
                self.advance();

                // Property access: n.prop
                if matches!(self.current(), Token::Dot) {
                    self.advance();
                    let prop = if let Token::Identifier(prop_name) = self.current() {
                        let prop_name = prop_name.clone();
                        self.advance();
                        prop_name
                    } else {
                        return Err(AtlantisError::Parse("expected property name after '.'".into()));
                    };
                    return Ok(Expression::Property(
                        Box::new(Expression::Variable(name)),
                        prop,
                    ));
                }

                // Function call: name(...)
                if matches!(self.current(), Token::LParen) {
                    self.advance();
                    let args = if matches!(self.current(), Token::RParen) {
                        Vec::new()
                    } else {
                        self.parse_expression_list()?
                    };
                    self.expect(&Token::RParen)?;
                    return Ok(Expression::FunctionCall(name, args));
                }

                Ok(Expression::Variable(name))
            }
            Token::LParen => {
                self.advance();
                let expr = self.parse_expression()?;
                self.expect(&Token::RParen)?;
                Ok(expr)
            }
            Token::LBracket => {
                // List literal
                self.advance();
                let mut items = Vec::new();
                if !matches!(self.current(), Token::RBracket) {
                    items = self.parse_expression_list()?;
                }
                self.expect(&Token::RBracket)?;
                Ok(Expression::List(items))
            }
            Token::LBrace => {
                // Map literal
                self.advance();
                let mut entries = Vec::new();
                loop {
                    if matches!(self.current(), Token::RBrace) {
                        self.advance();
                        break;
                    }
                    let key = if let Token::Identifier(name) = self.current() {
                        let name = name.clone();
                        self.advance();
                        name
                    } else {
                        return Err(AtlantisError::Parse("expected map key".into()));
                    };
                    self.expect(&Token::Colon)?;
                    let value = self.parse_expression()?;
                    entries.push((key, value));
                    if matches!(self.current(), Token::Comma) {
                        self.advance();
                    }
                }
                Ok(Expression::Map(entries))
            }
            token => Err(AtlantisError::Parse(format!(
                "unexpected token in expression: {token:?}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_match() {
        let stmt = parse("MATCH (n:Person) RETURN n").unwrap();
        match stmt {
            Statement::Query(q) => {
                assert!(!q.clauses.is_empty());
                assert!(q.result.is_some());
            }
        }
    }

    #[test]
    fn test_parse_match_with_relationship() {
        let stmt = parse("MATCH (a:Person)-[:KNOWS]->(b:Person) RETURN a, b").unwrap();
        match stmt {
            Statement::Query(q) => {
                assert!(q.clauses.len() >= 1);
            }
        }
    }

    #[test]
    fn test_parse_create() {
        let stmt = parse("CREATE (n:Person {name: 'Alice', age: 30}) RETURN n").unwrap();
        match stmt {
            Statement::Query(q) => {
                assert!(q.clauses.iter().any(|c| matches!(c, Clause::Create(_))));
                assert!(q.result.is_some());
            }
        }
    }

    #[test]
    fn test_parse_where_filter() {
        let stmt =
            parse("MATCH (n:Person) WHERE n.name = 'Alice' AND n.age > 25 RETURN n.name").unwrap();
        match stmt {
            Statement::Query(q) => {
                assert!(q.clauses.iter().any(|c| matches!(c, Clause::Match(_))));
                assert!(q.clauses.iter().any(|c| matches!(c, Clause::Where(_))));
            }
        }
    }

    #[test]
    fn test_parse_order_by_limit() {
        let stmt = parse("MATCH (n) RETURN n.name ORDER BY n.name DESC LIMIT 5").unwrap();
        match stmt {
            Statement::Query(q) => {
                let ret = q.result.unwrap();
                assert!(ret.order_by.is_some());
                assert!(ret.limit.is_some());
            }
        }
    }

    #[test]
    fn test_parse_parameterized() {
        let stmt = parse("MATCH (n:Person) WHERE n.name = $name RETURN n").unwrap();
        assert!(matches!(stmt, Statement::Query(_)));
    }

    #[test]
    fn test_parse_call_vector_search() {
        let stmt = parse(
            "CALL db.vector.search('Embedding', $query_vec, 10, 0.7) YIELD node, score RETURN node, score",
        )
        .unwrap();
        match stmt {
            Statement::Query(q) => {
                assert!(q.clauses.iter().any(|c| matches!(c, Clause::Call(_))));
            }
        }
    }

    #[test]
    fn test_parse_set() {
        let stmt = parse("MATCH (n:Person) SET n.age = 31 RETURN n").unwrap();
        match stmt {
            Statement::Query(q) => {
                assert!(q.clauses.iter().any(|c| matches!(c, Clause::Set(_))));
            }
        }
    }

    #[test]
    fn test_parse_delete() {
        let stmt = parse("MATCH (n:Person) DELETE n").unwrap();
        match stmt {
            Statement::Query(q) => {
                assert!(q.clauses.iter().any(|c| matches!(c, Clause::Delete(_))));
            }
        }
    }

    #[test]
    fn test_parse_detach_delete() {
        let stmt = parse("MATCH (n:Person) DETACH DELETE n").unwrap();
        match stmt {
            Statement::Query(q) => {
                assert!(q.clauses.iter().any(|c| {
                    matches!(c, Clause::Delete(DeleteClause { detach: true, .. }))
                }));
            }
        }
    }

    #[test]
    fn test_parse_with() {
        let stmt =
            parse("MATCH (n:Person) WITH n.name AS name RETURN name").unwrap();
        match stmt {
            Statement::Query(q) => {
                assert!(q.clauses.iter().any(|c| matches!(c, Clause::With(_))));
            }
        }
    }
}
