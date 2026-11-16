//! `CALL procedure(args) [YIELD ...] [WHERE ...]`.

use atlantis_core::{AtlantisError, Value};
use atlantis_cypher::ast::{Expression, YieldItem};

use crate::{
    exec::{eval::invalid, pushdown::pushable_conditions, Exec, QueryResult, Row},
    vector_index::FieldFilter,
};

/// Rows and the yielded column names (`None` when there were no input rows).
type CallOutput = (Vec<Row>, Option<Vec<String>>);

impl Exec<'_> {
    /// Runs the procedure once per row and binds its yielded columns. The
    /// column names are returned too, for a query that ends in `CALL`.
    pub(crate) fn call_clause(
        &mut self,
        rows: Vec<Row>,
        procedure: &str,
        args: &[Expression],
        yields: Option<&Vec<YieldItem>>,
        where_clause: Option<&Expression>,
    ) -> Result<CallOutput, AtlantisError> {
        let mut out = Vec::new();
        let mut columns_seen = None;
        // Conditions of the WHERE that the procedure can apply
        // itself (vector search filters inside the index). The
        // whole WHERE still runs on every row below.
        let pushed = match where_clause {
            Some(w) if procedure.eq_ignore_ascii_case("db.vector.search") => {
                pushable_conditions(w, yields)
            }
            _ => Vec::new(),
        };
        for row in &rows {
            let arg_values: Vec<Value> = args
                .iter()
                .map(|a| self.eval(a, row))
                .collect::<Result<_, _>>()?;
            let mut filter = Vec::with_capacity(pushed.len());
            for cond in &pushed {
                // A value that fails to evaluate is left to WHERE.
                if let Ok(value) = self.eval(cond.value, row) {
                    filter.push(FieldFilter {
                        field: cond.field.clone(),
                        op: cond.op,
                        value,
                        field_on_left: cond.field_on_left,
                    });
                }
            }
            let (columns, results) = self.call_procedure(procedure, arg_values, filter)?;
            let bindings = yield_bindings(&columns, yields)?;
            columns_seen = Some(
                bindings
                    .iter()
                    .map(|(_, name)| name.clone())
                    .collect::<Vec<_>>(),
            );
            for result in results {
                let mut r = row.clone();
                for (index, name) in &bindings {
                    r.insert(name.clone(), result[*index].clone());
                }
                if self.passes(where_clause, &r)? {
                    out.push(r);
                }
            }
        }
        Ok((out, columns_seen))
    }
}

/// The result of a query that ends in `CALL`: the yielded columns.
pub(crate) fn yielded_result(columns: Vec<String>, rows: &[Row]) -> QueryResult {
    let out = rows
        .iter()
        .map(|r| {
            columns
                .iter()
                .map(|c| r.get(c).cloned().unwrap_or(Value::Null))
                .collect()
        })
        .collect();
    QueryResult { columns, rows: out }
}

/// Maps yielded columns to (column index, bound name).
fn yield_bindings(
    columns: &[String],
    yields: Option<&Vec<YieldItem>>,
) -> Result<Vec<(usize, String)>, AtlantisError> {
    match yields {
        None => Ok(columns.iter().cloned().enumerate().collect()),
        Some(items) => items
            .iter()
            .map(|y| {
                let index = columns.iter().position(|c| c == &y.column).ok_or_else(|| {
                    invalid(format!(
                        "procedure has no output column {} (has: {})",
                        y.column,
                        columns.join(", ")
                    ))
                })?;
                Ok((index, y.alias.clone().unwrap_or_else(|| y.column.clone())))
            })
            .collect(),
    }
}
