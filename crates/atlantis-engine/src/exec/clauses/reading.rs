//! Reading clauses: `MATCH`, `OPTIONAL MATCH`, `WITH` and `UNWIND`.

use atlantis_core::{AtlantisError, Value};
use atlantis_cypher::ast::{Expression, Pattern, Projection};

use crate::exec::{pattern::pattern_variables, Exec, Row};

impl Exec<'_> {
    /// `[OPTIONAL] MATCH pattern [WHERE expr]`. An optional match keeps a
    /// row that matched nothing, with the pattern's new variables `null`.
    pub(crate) fn match_clause(
        &mut self,
        rows: Vec<Row>,
        optional: bool,
        pattern: &Pattern,
        where_clause: Option<&Expression>,
    ) -> Result<Vec<Row>, AtlantisError> {
        let new_vars: Vec<String> = pattern_variables(pattern);
        let mut out = Vec::new();
        for row in rows {
            let before = out.len();
            for m in self.match_pattern(pattern, &row)? {
                if self.passes(where_clause, &m)? {
                    out.push(m);
                }
            }
            if optional && out.len() == before {
                let mut r = row;
                for v in &new_vars {
                    r.entry(v.clone()).or_insert(Value::Null);
                }
                out.push(r);
            }
        }
        Ok(out)
    }

    /// `WITH projection [WHERE expr]`.
    pub(crate) fn with_clause(
        &mut self,
        rows: Vec<Row>,
        projection: &Projection,
        where_clause: Option<&Expression>,
    ) -> Result<Vec<Row>, AtlantisError> {
        let projected = self.project(projection, rows)?;
        let mut out = Vec::new();
        for row in projected.rows {
            if self.passes(where_clause, &row)? {
                out.push(row);
            }
        }
        Ok(out)
    }

    /// `UNWIND expr AS variable`: one row per list element; `null` yields
    /// no rows and a non-list value a single row.
    pub(crate) fn unwind_clause(
        &mut self,
        rows: Vec<Row>,
        expr: &Expression,
        variable: &str,
    ) -> Result<Vec<Row>, AtlantisError> {
        let mut out = Vec::new();
        for row in &rows {
            let items = match self.eval(expr, row)? {
                Value::Null => Vec::new(),
                Value::Array(items) => items,
                Value::Vector(v) => v.into_iter().map(|x| Value::Float(x as f64)).collect(),
                other => vec![other],
            };
            for item in items {
                let mut r = row.clone();
                r.insert(variable.to_string(), item);
                out.push(r);
            }
        }
        Ok(out)
    }
}
