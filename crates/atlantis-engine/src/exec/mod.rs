//! The Cypher executor: interprets the AST clause by clause against a
//! record transaction.
//!
//! Every clause maps a list of rows (variable → value) to a new list. Nodes
//! and relationships are bound as `Value::RecordId`; reading a property of
//! one loads the record inside the same transaction, so a query sees its own
//! writes. `RETURN` turns variables bound to records into documents.
//!
//! Layout: [`clauses`] holds one module per clause family, [`projection`]
//! and [`aggregate`] implement `RETURN`/`WITH`, [`procedures`] the `CALL`
//! targets, [`pushdown`] the vector-search filter push-down, [`ddl`] the
//! index statements, [`eval`] expressions and [`pattern`] pattern matching.

mod aggregate;
mod clauses;
mod ddl;
pub(crate) mod eval;
mod pattern;
mod procedures;
mod projection;
mod pushdown;

use std::collections::{HashMap, HashSet};

use atlantis_core::{AtlantisError, Value};
use atlantis_cypher::ast::{Clause, Query, QueryUnion, Statement};
use atlantis_storage::{RecordStore, RecordTx};

use crate::vector_index::VectorIndexes;
use eval::{group_key, invalid};

/// Variable bindings of one row.
pub(crate) type Row = HashMap<String, Value>;

/// Query parameters (`$name` → value).
pub type Params = HashMap<String, Value>;

/// Columns and rows produced by a statement.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// Executes statements inside one record transaction.
pub(crate) struct Exec<'a> {
    pub(crate) tx: &'a mut RecordTx,
    pub(crate) params: &'a Params,
    pub(crate) store: &'a RecordStore,
    pub(crate) vectors: &'a VectorIndexes,
    pub(crate) vector_defaults: &'a atlantis_core::VectorConfig,
    /// Index definitions created or dropped, to invalidate after commit.
    pub(crate) index_changes: Vec<String>,
    /// Compiled `=~` patterns, reused across rows.
    pub(crate) regexes: HashMap<String, regex::Regex>,
}

impl<'a> Exec<'a> {
    pub(crate) fn run(&mut self, statement: &Statement) -> Result<QueryResult, AtlantisError> {
        match statement {
            Statement::Query(union) => self.run_union(union),
            Statement::CreateVectorIndex(def) => {
                self.create_vector_index(def)?;
                Ok(QueryResult::default())
            }
            Statement::DropIndex { name, if_exists } => {
                self.drop_index(name, *if_exists)?;
                Ok(QueryResult::default())
            }
            Statement::Begin | Statement::Commit | Statement::Rollback => Err(invalid(
                "transaction control statements are handled by the session",
            )),
        }
    }

    fn run_union(&mut self, union: &QueryUnion) -> Result<QueryResult, AtlantisError> {
        let mut result = self.run_query(&union.first)?;
        let mut dedupe = false;
        for (all, query) in &union.rest {
            let next = self.run_query(query)?;
            if next.columns != result.columns {
                return Err(invalid(
                    "all queries in a UNION must return the same columns",
                ));
            }
            result.rows.extend(next.rows);
            dedupe |= !all;
        }
        if dedupe {
            let mut seen = HashSet::new();
            result.rows.retain(|row| seen.insert(group_key(row)));
        }
        Ok(result)
    }

    fn run_query(&mut self, query: &Query) -> Result<QueryResult, AtlantisError> {
        let mut rows = vec![Row::new()];
        let mut call_columns: Option<Vec<String>> = None;
        for clause in &query.clauses {
            call_columns = None;
            rows = match clause {
                Clause::Match {
                    optional,
                    pattern,
                    where_clause,
                } => self.match_clause(rows, *optional, pattern, where_clause.as_ref())?,
                Clause::Create(pattern) => self.create_clause(rows, pattern)?,
                Clause::Merge {
                    path,
                    on_create,
                    on_match,
                } => self.merge_clause(rows, path, on_create, on_match)?,
                Clause::Set(items) => self.set_clause(rows, items)?,
                Clause::Remove(items) => self.remove_clause(rows, items)?,
                Clause::Delete { detach, exprs } => self.delete_clause(rows, *detach, exprs)?,
                Clause::With {
                    projection,
                    where_clause,
                } => self.with_clause(rows, projection, where_clause.as_ref())?,
                Clause::Unwind { expr, variable } => self.unwind_clause(rows, expr, variable)?,
                Clause::Call {
                    procedure,
                    args,
                    yields,
                    where_clause,
                } => {
                    let (out, columns) = self.call_clause(
                        rows,
                        procedure,
                        args,
                        yields.as_ref(),
                        where_clause.as_ref(),
                    )?;
                    call_columns = columns;
                    out
                }
            };
        }
        match &query.ret {
            Some(projection) => self.return_clause(projection, rows),
            // A query ending in CALL returns the yielded columns.
            None => Ok(match call_columns {
                Some(columns) => clauses::yielded_result(columns, &rows),
                None => QueryResult::default(),
            }),
        }
    }
}
