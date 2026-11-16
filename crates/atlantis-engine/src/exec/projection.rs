//! Projection for `RETURN` and `WITH`: items and `*`, grouping with
//! aggregates, `DISTINCT`, `ORDER BY`, `SKIP` and `LIMIT`, and turning
//! returned records into documents.

use std::collections::{HashMap, HashSet};

use atlantis_core::{AtlantisError, Value};
use atlantis_cypher::ast::{Expression, Projection};

use super::{
    eval::{group_key, invalid, order_cmp},
    Exec, QueryResult, Row,
};

/// Output of a projection: column names and rows (as bindings, in order).
pub(super) struct Projected {
    pub(super) columns: Vec<String>,
    pub(super) rows: Vec<Row>,
}

impl Exec<'_> {
    /// `RETURN projection`: projects the rows, then expands the columns that
    /// return records into documents.
    pub(super) fn return_clause(
        &mut self,
        projection: &Projection,
        rows: Vec<Row>,
    ) -> Result<QueryResult, AtlantisError> {
        let projected = self.project(projection, rows)?;
        // Columns from `*` are variables; then one flag per item.
        let star_columns = projected.columns.len() - projection.items.len();
        let expand: Vec<bool> = std::iter::repeat_n(true, star_columns)
            .chain(projection.items.iter().map(|i| returns_records(&i.expr)))
            .collect();
        let mut out = Vec::with_capacity(projected.rows.len());
        for row in &projected.rows {
            let mut values = Vec::with_capacity(projected.columns.len());
            for (i, col) in projected.columns.iter().enumerate() {
                let v = row.get(col).cloned().unwrap_or(Value::Null);
                values.push(if expand.get(i).copied().unwrap_or(false) {
                    self.expand(v)?
                } else {
                    v
                });
            }
            out.push(values);
        }
        Ok(QueryResult {
            columns: projected.columns,
            rows: out,
        })
    }

    /// Turns a record id into its document (recursively inside lists).
    fn expand(&mut self, v: Value) -> Result<Value, AtlantisError> {
        Ok(match v {
            Value::RecordId(id) => match self.load(&id)? {
                Some(record) => {
                    let mut obj = record.data;
                    obj.insert("id".into(), Value::RecordId(record.id));
                    Value::Object(obj)
                }
                None => Value::Null,
            },
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .map(|i| self.expand(i))
                    .collect::<Result<_, _>>()?,
            ),
            other => other,
        })
    }

    fn eval_count(
        &mut self,
        expr: Option<&Expression>,
        row: &Row,
        what: &str,
    ) -> Result<Option<usize>, AtlantisError> {
        let Some(expr) = expr else { return Ok(None) };
        match self.eval(expr, row)? {
            Value::Integer(i) if i >= 0 => Ok(Some(i as usize)),
            other => Err(invalid(format!(
                "{what} must be a non-negative integer, got {other}"
            ))),
        }
    }

    pub(super) fn project(
        &mut self,
        projection: &Projection,
        rows: Vec<Row>,
    ) -> Result<Projected, AtlantisError> {
        let mut columns: Vec<String> = Vec::new();
        // Variables kept by `*`; with aggregates they are grouping keys.
        let mut star_vars: Vec<String> = Vec::new();
        if projection.star {
            star_vars = rows
                .first()
                .map(|r| r.keys().cloned().collect())
                .unwrap_or_default();
            star_vars.sort();
            columns.extend(star_vars.iter().cloned());
        }
        for item in &projection.items {
            columns.push(item.name().to_string());
        }
        let aggregating = projection.items.iter().any(|i| i.expr.contains_aggregate());
        // The ORDER BY context is only read when there is an ORDER BY.
        let ordered = !projection.order_by.is_empty();

        // Each output row keeps a context for ORDER BY: the projected values,
        // plus (when not aggregating) the variables of the input row.
        let mut out: Vec<(Row, Row)> = Vec::new();
        if aggregating {
            let keys: Vec<usize> = (0..projection.items.len())
                .filter(|i| !projection.items[*i].expr.contains_aggregate())
                .collect();
            let mut groups: Vec<(Vec<Value>, Vec<Row>)> = Vec::new();
            let mut index: HashMap<String, usize> = HashMap::new();
            for row in rows {
                let mut key_values: Vec<Value> = keys
                    .iter()
                    .map(|i| self.eval(&projection.items[*i].expr, &row))
                    .collect::<Result<_, _>>()?;
                key_values.extend(
                    star_vars
                        .iter()
                        .map(|v| row.get(v).cloned().unwrap_or(Value::Null)),
                );
                let k = group_key(&key_values);
                match index.get(&k) {
                    Some(&g) => groups[g].1.push(row),
                    None => {
                        index.insert(k, groups.len());
                        groups.push((key_values, vec![row]));
                    }
                }
            }
            if groups.is_empty() && keys.is_empty() && star_vars.is_empty() {
                groups.push((Vec::new(), Vec::new()));
            }
            let empty = Row::new();
            for (_, group) in groups {
                let first = group.first().unwrap_or(&empty);
                let mut projected = Row::new();
                for var in &star_vars {
                    projected.insert(var.clone(), first.get(var).cloned().unwrap_or(Value::Null));
                }
                for item in &projection.items {
                    let value = if item.expr.contains_aggregate() {
                        let substituted = self.substitute_aggregates(&item.expr, &group)?;
                        self.eval(&substituted, first)?
                    } else {
                        self.eval(&item.expr, first)?
                    };
                    projected.insert(item.name().to_string(), value);
                }
                let context = if ordered {
                    projected.clone()
                } else {
                    Row::new()
                };
                out.push((projected, context));
            }
        } else {
            for row in rows {
                let mut projected = Row::new();
                if projection.star {
                    projected.extend(row.clone());
                }
                for item in &projection.items {
                    projected.insert(item.name().to_string(), self.eval(&item.expr, &row)?);
                }
                let context = if ordered {
                    let mut context = row;
                    context.extend(projected.clone());
                    context
                } else {
                    Row::new()
                };
                out.push((projected, context));
            }
        }

        if projection.distinct {
            let mut seen = HashSet::new();
            out.retain(|(p, _)| {
                let values: Vec<Value> = columns
                    .iter()
                    .map(|c| p.get(c).cloned().unwrap_or(Value::Null))
                    .collect();
                seen.insert(group_key(&values))
            });
        }

        if ordered {
            let mut keyed = Vec::with_capacity(out.len());
            for (projected, context) in out {
                let mut keys = Vec::with_capacity(projection.order_by.len());
                for order in &projection.order_by {
                    // An ORDER BY expression equal to a projected item reuses its value.
                    let same = projection.items.iter().find(|i| i.expr == order.expr);
                    keys.push(match same {
                        Some(item) => projected.get(item.name()).cloned().unwrap_or(Value::Null),
                        None => self.eval(&order.expr, &context)?,
                    });
                }
                keyed.push((keys, projected, context));
            }
            keyed.sort_by(|a, b| {
                for (i, order) in projection.order_by.iter().enumerate() {
                    let mut c = order_cmp(&a.0[i], &b.0[i]);
                    if order.descending {
                        c = c.reverse();
                    }
                    if c != std::cmp::Ordering::Equal {
                        return c;
                    }
                }
                std::cmp::Ordering::Equal
            });
            out = keyed.into_iter().map(|(_, p, c)| (p, c)).collect();
        }

        let skip = self
            .eval_count(projection.skip.as_ref(), &Row::new(), "SKIP")?
            .unwrap_or(0);
        let limit = self.eval_count(projection.limit.as_ref(), &Row::new(), "LIMIT")?;
        let rows = out
            .into_iter()
            .skip(skip)
            .take(limit.unwrap_or(usize::MAX))
            .map(|(p, _)| p)
            .collect();
        Ok(Projected { columns, rows })
    }
}

/// `true` when a RETURN item yields records that should be returned as
/// documents: a bare variable, or `collect(variable)`.
fn returns_records(expr: &Expression) -> bool {
    match expr {
        Expression::Variable(_) => true,
        Expression::Function { name, args, .. } if name.eq_ignore_ascii_case("collect") => {
            matches!(args.as_slice(), [Expression::Variable(_)])
        }
        _ => false,
    }
}
