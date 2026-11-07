//! Query clauses and the `RETURN`/`WITH` projection body.

use super::ast::{
    Clause, Expression, OrderItem, Projection, ProjectionItem, Query, RemoveItem, SetItem,
    YieldItem,
};
use super::cursor::{PResult, Parser};
use crate::lexer::Token;

impl Parser<'_> {
    /// One query part: clauses, then an optional `RETURN`. Only `UNION`,
    /// `;` or end of input may follow.
    pub(crate) fn query(&mut self) -> PResult<Query> {
        let mut clauses = Vec::new();
        while let Some(clause) = self.clause()? {
            clauses.push(clause);
        }
        let ret = if self.eat_kw("RETURN") {
            Some(self.projection()?)
        } else {
            None
        };
        if clauses.is_empty() && ret.is_none() {
            return Err(self.unexpected("a clause such as MATCH, CREATE or RETURN"));
        }
        if !(self.at_kw("UNION") || self.at(&Token::Semicolon) || self.at(&Token::Eof)) {
            let expected = if ret.is_some() {
                "UNION, ';' or end of input after RETURN"
            } else {
                "a clause, RETURN, UNION, ';' or end of input"
            };
            return Err(self.unexpected(expected));
        }
        Ok(Query { clauses, ret })
    }

    /// The next clause, or `None` if the cursor is not at a clause keyword.
    fn clause(&mut self) -> PResult<Option<Clause>> {
        let tok = self.peek().clone();
        let kw = |k: &str| tok.is_keyword(k);
        let clause = if kw("MATCH") || kw("OPTIONAL") {
            let optional = self.eat_kw("OPTIONAL");
            self.expect_kw("MATCH")?;
            let pattern = self.pattern()?;
            Clause::Match {
                optional,
                pattern,
                where_clause: self.opt_where()?,
            }
        } else if kw("CREATE") {
            self.bump();
            Clause::Create(self.pattern()?)
        } else if kw("MERGE") {
            self.bump();
            self.merge()?
        } else if kw("SET") {
            self.bump();
            Clause::Set(self.set_items()?)
        } else if kw("REMOVE") {
            self.bump();
            Clause::Remove(self.comma_list(Self::remove_item)?)
        } else if kw("DETACH") || kw("DELETE") {
            let detach = self.eat_kw("DETACH");
            self.expect_kw("DELETE")?;
            Clause::Delete {
                detach,
                exprs: self.comma_list(Self::expression)?,
            }
        } else if kw("WITH") {
            self.bump();
            let projection = self.projection()?;
            Clause::With {
                projection,
                where_clause: self.opt_where()?,
            }
        } else if kw("UNWIND") {
            self.bump();
            let expr = self.expression()?;
            self.expect_kw("AS")?;
            Clause::Unwind {
                expr,
                variable: self.name("a variable")?,
            }
        } else if kw("CALL") {
            self.bump();
            self.call()?
        } else {
            return Ok(None);
        };
        Ok(Some(clause))
    }

    /// One or more `item`s separated by commas.
    fn comma_list<T>(&mut self, item: fn(&mut Self) -> PResult<T>) -> PResult<Vec<T>> {
        let mut items = vec![item(self)?];
        while self.eat(&Token::Comma) {
            items.push(item(self)?);
        }
        Ok(items)
    }

    fn opt_where(&mut self) -> PResult<Option<Expression>> {
        if self.eat_kw("WHERE") {
            Ok(Some(self.expression()?))
        } else {
            Ok(None)
        }
    }

    /// After `MERGE`: a path, then `ON CREATE SET` / `ON MATCH SET` in any
    /// order, each repeatable.
    fn merge(&mut self) -> PResult<Clause> {
        let path = self.path_pattern()?;
        let (mut on_create, mut on_match) = (Vec::new(), Vec::new());
        while self.eat_kw("ON") {
            let target = if self.eat_kw("CREATE") {
                &mut on_create
            } else if self.eat_kw("MATCH") {
                &mut on_match
            } else {
                return Err(self.unexpected("CREATE or MATCH after ON"));
            };
            self.expect_kw("SET")?;
            target.extend(self.set_items()?);
        }
        Ok(Clause::Merge {
            path,
            on_create,
            on_match,
        })
    }

    fn set_items(&mut self) -> PResult<Vec<SetItem>> {
        self.comma_list(Self::set_item)
    }

    /// `n.a.b = e`, `n = e`, `n += e` or `n:L1:L2`.
    fn set_item(&mut self) -> PResult<SetItem> {
        let variable = self.name("a variable")?;
        match self.peek() {
            Token::Dot => {
                let path = self.property_path()?;
                self.expect(&Token::Eq, "'='")?;
                Ok(SetItem::Property {
                    variable,
                    path,
                    value: self.expression()?,
                })
            }
            Token::Eq => {
                self.bump();
                Ok(SetItem::Replace {
                    variable,
                    value: self.expression()?,
                })
            }
            Token::PlusEq => {
                self.bump();
                Ok(SetItem::Merge {
                    variable,
                    value: self.expression()?,
                })
            }
            Token::Colon => Ok(SetItem::Labels {
                variable,
                labels: self.label_list()?,
            }),
            _ => Err(self.unexpected("'.', '=', '+=' or ':'")),
        }
    }

    /// `n.a.b` or `n:L1:L2`.
    fn remove_item(&mut self) -> PResult<RemoveItem> {
        let variable = self.name("a variable")?;
        match self.peek() {
            Token::Dot => Ok(RemoveItem::Property {
                variable,
                path: self.property_path()?,
            }),
            Token::Colon => Ok(RemoveItem::Labels {
                variable,
                labels: self.label_list()?,
            }),
            _ => Err(self.unexpected("'.' or ':'")),
        }
    }

    /// `.a.b` — one or more property keys.
    fn property_path(&mut self) -> PResult<Vec<String>> {
        let mut path = Vec::new();
        while self.eat(&Token::Dot) {
            path.push(self.name("a property key")?);
        }
        if path.is_empty() {
            return Err(self.unexpected("'.'"));
        }
        Ok(path)
    }

    /// After `CALL`: `a.b.c[(args)] [YIELD ...] [WHERE e]`.
    fn call(&mut self) -> PResult<Clause> {
        let mut procedure = self.name("a procedure name")?;
        while self.eat(&Token::Dot) {
            procedure.push('.');
            procedure.push_str(&self.name("a procedure name")?);
        }
        let args = if self.eat(&Token::LParen) {
            self.call_args()?
        } else {
            Vec::new()
        };
        let yields = if !self.eat_kw("YIELD") || self.eat(&Token::Star) {
            None
        } else {
            Some(self.comma_list(Self::yield_item)?)
        };
        Ok(Clause::Call {
            procedure,
            args,
            yields,
            where_clause: self.opt_where()?,
        })
    }

    fn yield_item(&mut self) -> PResult<YieldItem> {
        let column = self.name("a column name")?;
        let alias = if self.eat_kw("AS") {
            Some(self.name("an alias")?)
        } else {
            None
        };
        Ok(YieldItem { column, alias })
    }

    /// `[DISTINCT] * | items [ORDER BY ...] [SKIP e] [LIMIT e]`; `SKIP` and
    /// `LIMIT` may come in either order.
    pub(crate) fn projection(&mut self) -> PResult<Projection> {
        let distinct = self.eat_kw("DISTINCT");
        let star = self.eat(&Token::Star);
        let items = if !star || self.eat(&Token::Comma) {
            self.comma_list(Self::projection_item)?
        } else {
            Vec::new()
        };
        let order_by = if self.eat_kw("ORDER") {
            self.expect_kw("BY")?;
            self.comma_list(Self::order_item)?
        } else {
            Vec::new()
        };
        let (mut skip, mut limit) = (None, None);
        loop {
            let slot = if self.at_kw("SKIP") {
                &mut skip
            } else if self.at_kw("LIMIT") {
                &mut limit
            } else {
                break;
            };
            if slot.is_some() {
                return Err(self.unexpected("at most one SKIP and one LIMIT"));
            }
            self.bump();
            *slot = Some(self.expression()?);
        }
        Ok(Projection {
            distinct,
            star,
            items,
            order_by,
            skip,
            limit,
        })
    }

    fn projection_item(&mut self) -> PResult<ProjectionItem> {
        let start = self.offset();
        let expr = self.expression()?;
        let text = self.text(start, self.prev_end());
        let alias = if self.eat_kw("AS") {
            Some(self.name("an alias")?)
        } else {
            None
        };
        Ok(ProjectionItem { expr, alias, text })
    }

    fn order_item(&mut self) -> PResult<OrderItem> {
        let expr = self.expression()?;
        let descending = ["DESC", "DESCENDING"].iter().any(|kw| self.eat_kw(kw));
        if !descending && !self.eat_kw("ASC") {
            self.eat_kw("ASCENDING");
        }
        Ok(OrderItem { expr, descending })
    }
}
