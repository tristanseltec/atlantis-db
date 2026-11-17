//! Property index and constraint DDL in the Neo4j 5 forms, plus `DROP`
//! and `SHOW`:
//!
//! ```text
//! CREATE [RANGE] INDEX [name] [IF NOT EXISTS] FOR (n:Label) ON (n.a, n.b.c)
//! CREATE CONSTRAINT [name] [IF NOT EXISTS] FOR (n:Label)
//!     REQUIRE n.a IS [NODE] UNIQUE
//!     REQUIRE (n.a, n.b) IS [NODE] UNIQUE
//!     REQUIRE n.a IS NOT NULL
//!     REQUIRE (n.a, n.b) IS [NODE] KEY
//! DROP INDEX name [IF EXISTS]
//! DROP CONSTRAINT name [IF EXISTS]
//! SHOW [ALL] INDEX[ES]
//! SHOW [ALL] CONSTRAINT[S]
//! ```

use super::ast::{ConstraintRequirement, CreateConstraint, CreateIndex, Statement};
use super::cursor::{PResult, Parser};
use crate::lexer::Token;

/// Index kinds Neo4j names that Atlantis does not implement yet.
const UNSUPPORTED_INDEX_KINDS: &[&str] = &["TEXT", "POINT", "FULLTEXT", "LOOKUP"];

impl Parser<'_> {
    /// `true` at `CREATE [RANGE|TEXT|...] INDEX` or `CREATE CONSTRAINT`.
    pub(super) fn is_schema_create(&self) -> bool {
        let next = self.peek_nth(1);
        next.is_keyword("INDEX")
            || next.is_keyword("CONSTRAINT")
            || ((next.is_keyword("RANGE")
                || UNSUPPORTED_INDEX_KINDS.iter().any(|k| next.is_keyword(k)))
                && self.peek_nth(2).is_keyword("INDEX"))
    }

    /// After `CREATE`: an index or a constraint.
    pub(super) fn create_schema(&mut self) -> PResult<Statement> {
        self.expect_kw("CREATE")?;
        if self.eat_kw("CONSTRAINT") {
            return self.create_constraint().map(Statement::CreateConstraint);
        }
        if let Some(kind) = UNSUPPORTED_INDEX_KINDS.iter().find(|k| self.at_kw(k)) {
            return Err(self.error_at(
                self.offset(),
                &format!("{kind} indexes are not supported yet; use a RANGE index"),
            ));
        }
        self.eat_kw("RANGE");
        self.expect_kw("INDEX")?;
        let (name, if_not_exists) = self.schema_name()?;
        let (variable, label) = self.schema_target()?;
        self.expect_kw("ON")?;
        self.expect(&Token::LParen, "'('")?;
        let mut properties = vec![self.schema_property(&variable)?];
        while self.eat(&Token::Comma) {
            properties.push(self.schema_property(&variable)?);
        }
        self.expect(&Token::RParen, "')'")?;
        Ok(Statement::CreateIndex(CreateIndex {
            name,
            if_not_exists,
            label,
            properties,
        }))
    }

    /// After `CREATE CONSTRAINT`.
    fn create_constraint(&mut self) -> PResult<CreateConstraint> {
        let (name, if_not_exists) = self.schema_name()?;
        let (variable, label) = self.schema_target()?;
        self.expect_kw("REQUIRE")?;
        let properties = if self.eat(&Token::LParen) {
            let mut props = vec![self.schema_property(&variable)?];
            while self.eat(&Token::Comma) {
                props.push(self.schema_property(&variable)?);
            }
            self.expect(&Token::RParen, "')'")?;
            props
        } else {
            vec![self.schema_property(&variable)?]
        };
        self.expect_kw("IS")?;
        let offset = self.offset();
        let requirement = if self.eat_kw("NOT") {
            self.expect_kw("NULL")?;
            ConstraintRequirement::NotNull
        } else {
            let node = self.eat_kw("NODE");
            if self.eat_kw("UNIQUE") {
                ConstraintRequirement::Unique
            } else if self.eat_kw("KEY") {
                ConstraintRequirement::NodeKey
            } else if node {
                return Err(self.unexpected("UNIQUE or KEY"));
            } else {
                return Err(self.unexpected("UNIQUE, NOT NULL, NODE KEY or KEY"));
            }
        };
        if requirement == ConstraintRequirement::NotNull && properties.len() != 1 {
            return Err(self.error_at(offset, "IS NOT NULL takes exactly one property"));
        }
        Ok(CreateConstraint {
            name,
            if_not_exists,
            label,
            properties,
            requirement,
        })
    }

    /// `[name] [IF NOT EXISTS]` before `FOR`. A name that is a keyword
    /// (`FOR`, `IF`) must be quoted with backticks.
    fn schema_name(&mut self) -> PResult<(Option<String>, bool)> {
        let name = if self.at_kw("FOR") || self.at_kw("IF") {
            None
        } else {
            Some(self.name("a name, IF NOT EXISTS or FOR")?)
        };
        let if_not_exists = self.eat_kw("IF");
        if if_not_exists {
            self.expect_kw("NOT")?;
            self.expect_kw("EXISTS")?;
        }
        Ok((name, if_not_exists))
    }

    /// `FOR (n:Label)`; returns the variable and the label.
    fn schema_target(&mut self) -> PResult<(String, String)> {
        self.expect_kw("FOR")?;
        self.expect(&Token::LParen, "'('")?;
        if self.at(&Token::RParen) {
            return Err(self.error_at(
                self.offset(),
                "relationship indexes and constraints are not supported yet",
            ));
        }
        let variable = self.name("a variable")?;
        self.expect(&Token::Colon, "':'")?;
        let label = self.name("a label")?;
        if self.at(&Token::Colon) {
            return Err(self.error_at(self.offset(), "an index or constraint covers one label"));
        }
        self.expect(&Token::RParen, "')'")?;
        Ok((variable, label))
    }

    /// `n.prop` or `n.nested.prop`, where `n` must be `variable`.
    fn schema_property(&mut self, variable: &str) -> PResult<Vec<String>> {
        let offset = self.offset();
        let owner = self.name("a variable")?;
        if owner != variable {
            return Err(self.error_at(
                offset,
                &format!("expected variable '{variable}', found '{owner}'"),
            ));
        }
        self.expect(&Token::Dot, "'.'")?;
        let mut path = vec![self.name("a property key")?];
        while self.eat(&Token::Dot) {
            path.push(self.name("a property key")?);
        }
        Ok(path)
    }

    /// After `DROP`: `INDEX name [IF EXISTS]` or `CONSTRAINT name [IF EXISTS]`.
    pub(super) fn drop_schema(&mut self) -> PResult<Statement> {
        let constraint = if self.eat_kw("CONSTRAINT") {
            true
        } else if self.eat_kw("INDEX") {
            false
        } else {
            return Err(self.unexpected("INDEX or CONSTRAINT"));
        };
        let name = self.name("a name")?;
        let if_exists = self.eat_kw("IF");
        if if_exists {
            self.expect_kw("EXISTS")?;
        }
        Ok(if constraint {
            Statement::DropConstraint { name, if_exists }
        } else {
            Statement::DropIndex { name, if_exists }
        })
    }

    /// After `SHOW`: `[ALL] INDEX[ES]` or `[ALL] CONSTRAINT[S]`.
    pub(super) fn show(&mut self) -> PResult<Statement> {
        self.eat_kw("ALL");
        if self.eat_kw("INDEX") || self.eat_kw("INDEXES") {
            Ok(Statement::ShowIndexes)
        } else if self.eat_kw("CONSTRAINT") || self.eat_kw("CONSTRAINTS") {
            Ok(Statement::ShowConstraints)
        } else {
            Err(self.unexpected("INDEXES or CONSTRAINTS"))
        }
    }
}
