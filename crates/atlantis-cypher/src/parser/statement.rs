//! Top-level statements: queries with `UNION`, `EXPLAIN`, vector index DDL
//! and transaction control. Property index and constraint DDL is in `ddl`.

use super::ast::{CreateVectorIndex, QueryUnion, Statement};
use super::cursor::{PResult, Parser};
use crate::lexer::Token;

impl Parser<'_> {
    pub(crate) fn statement(&mut self) -> PResult<Statement> {
        if self.at_kw("CREATE") && self.peek_nth(1).is_keyword("VECTOR") {
            return self.create_vector_index().map(Statement::CreateVectorIndex);
        }
        if self.at_kw("CREATE") && self.is_schema_create() {
            return self.create_schema();
        }
        if self.eat_kw("DROP") {
            return self.drop_schema();
        }
        if self.eat_kw("SHOW") {
            return self.show();
        }
        if self.eat_kw("EXPLAIN") {
            return Ok(Statement::Explain(self.query_union()?));
        }
        for (kw, stmt) in [
            ("BEGIN", Statement::Begin),
            ("COMMIT", Statement::Commit),
            ("ROLLBACK", Statement::Rollback),
        ] {
            if self.eat_kw(kw) {
                return Ok(stmt);
            }
        }
        self.query_union().map(Statement::Query)
    }

    pub(super) fn query_union(&mut self) -> PResult<QueryUnion> {
        let first = self.query()?;
        let mut rest = Vec::new();
        while self.eat_kw("UNION") {
            let all = self.eat_kw("ALL");
            rest.push((all, self.query()?));
        }
        Ok(QueryUnion { first, rest })
    }

    /// `CREATE VECTOR INDEX name [IF NOT EXISTS] FOR (n:Label) ON (n.prop)
    /// [OPTIONS {map}]`
    fn create_vector_index(&mut self) -> PResult<CreateVectorIndex> {
        self.expect_kw("CREATE")?;
        self.expect_kw("VECTOR")?;
        self.expect_kw("INDEX")?;
        if self.at_kw("IF") && self.peek_nth(1).is_keyword("NOT") {
            return Err(self.unexpected("an index name"));
        }
        let name = self.name("an index name")?;
        let if_not_exists = self.eat_kw("IF");
        if if_not_exists {
            self.expect_kw("NOT")?;
            self.expect_kw("EXISTS")?;
        }
        self.expect_kw("FOR")?;
        self.expect(&Token::LParen, "'('")?;
        let variable = self.name("a variable")?;
        self.expect(&Token::Colon, "':'")?;
        let label = self.name("a label")?;
        self.expect(&Token::RParen, "')'")?;
        self.expect_kw("ON")?;
        self.expect(&Token::LParen, "'('")?;
        let offset = self.offset();
        let owner = self.name("a variable")?;
        if owner != variable {
            return Err(self.error_at(
                offset,
                &format!("expected variable '{variable}', found '{owner}'"),
            ));
        }
        self.expect(&Token::Dot, "'.'")?;
        let property = self.name("a property key")?;
        self.expect(&Token::RParen, "')'")?;
        let options = if self.eat_kw("OPTIONS") {
            self.map_literal()?
        } else {
            Vec::new()
        };
        Ok(CreateVectorIndex {
            name,
            if_not_exists,
            label,
            property,
            options,
        })
    }
}
