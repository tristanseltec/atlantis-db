//! Graph patterns: `[p =] (a:L {k: v})-[r:T *1..3]->(b), ...`.

use atlantis_core::Direction;

use super::ast::{NodePattern, PathLength, PathPattern, Pattern, RelPattern};
use super::cursor::{PResult, Parser};
use crate::lexer::Token;

impl Parser<'_> {
    /// Comma-separated path patterns.
    pub(crate) fn pattern(&mut self) -> PResult<Pattern> {
        let mut paths = vec![self.path_pattern()?];
        while self.eat(&Token::Comma) {
            paths.push(self.path_pattern()?);
        }
        Ok(Pattern { paths })
    }

    pub(crate) fn path_pattern(&mut self) -> PResult<PathPattern> {
        let variable = if self.is_name_at(0) && self.peek_nth(1) == &Token::Eq {
            let name = self.name("a path variable")?;
            self.bump();
            Some(name)
        } else {
            None
        };
        let start = self.node_pattern()?;
        let mut steps = Vec::new();
        while matches!(self.peek(), Token::Minus | Token::Lt) {
            let rel = self.rel_pattern()?;
            steps.push((rel, self.node_pattern()?));
        }
        Ok(PathPattern {
            variable,
            start,
            steps,
        })
    }

    fn node_pattern(&mut self) -> PResult<NodePattern> {
        self.expect(&Token::LParen, "'(' to start a node pattern")?;
        let variable = if self.is_name_at(0) {
            Some(self.name("a variable")?)
        } else {
            None
        };
        let labels = if self.at(&Token::Colon) {
            self.label_list()?
        } else {
            Vec::new()
        };
        let properties = if self.at(&Token::LBrace) {
            self.map_literal()?
        } else {
            Vec::new()
        };
        self.expect(&Token::RParen, "')' to close the node pattern")?;
        Ok(NodePattern {
            variable,
            labels,
            properties,
        })
    }

    /// `-[...]->`, `<-[...]-`, `-[...]-` and the bracket-less forms.
    fn rel_pattern(&mut self) -> PResult<RelPattern> {
        let left = self.eat(&Token::Lt);
        self.expect(&Token::Minus, "'-'")?;
        let mut rel = if self.at(&Token::LBracket) {
            self.rel_detail()?
        } else {
            RelPattern {
                variable: None,
                types: Vec::new(),
                direction: Direction::Both,
                properties: Vec::new(),
                length: None,
            }
        };
        self.expect(&Token::Minus, "'-'")?;
        let right = self.eat(&Token::Gt);
        rel.direction = match (left, right) {
            (true, false) => Direction::Incoming,
            (false, true) => Direction::Outgoing,
            _ => Direction::Both,
        };
        Ok(rel)
    }

    /// `[var:T1|T2 *len {props}]`; the direction is filled in by the caller.
    fn rel_detail(&mut self) -> PResult<RelPattern> {
        self.expect(&Token::LBracket, "'['")?;
        let variable = if self.is_name_at(0) {
            Some(self.name("a variable")?)
        } else {
            None
        };
        let mut types = Vec::new();
        if self.eat(&Token::Colon) {
            types.push(self.name("a relationship type")?);
            while self.eat(&Token::Pipe) {
                self.eat(&Token::Colon);
                types.push(self.name("a relationship type")?);
            }
        }
        let length = if self.eat(&Token::Star) {
            Some(self.path_length()?)
        } else {
            None
        };
        let properties = if self.at(&Token::LBrace) {
            self.map_literal()?
        } else {
            Vec::new()
        };
        self.expect(&Token::RBracket, "']' to close the relationship pattern")?;
        Ok(RelPattern {
            variable,
            types,
            direction: Direction::Both,
            properties,
            length,
        })
    }

    /// After `*`: nothing, `n`, `a..b`, `..b` or `a..`.
    fn path_length(&mut self) -> PResult<PathLength> {
        let min = self.opt_length_bound()?;
        if self.eat(&Token::DotDot) {
            let max = self.opt_length_bound()?;
            return Ok(PathLength { min, max });
        }
        Ok(PathLength { min, max: min })
    }

    fn opt_length_bound(&mut self) -> PResult<Option<usize>> {
        let Token::Integer(n) = *self.peek() else {
            return Ok(None);
        };
        let offset = self.offset();
        self.bump();
        usize::try_from(n)
            .map(Some)
            .map_err(|_| self.error_at(offset, &format!("path length out of range: {n}")))
    }
}
