//! `EXPLAIN query`: the operators a query runs, in execution order, with
//! the access path chosen for the start node of every pattern. Nothing is
//! executed. Columns: `operator`, `details`.

use std::collections::HashSet;

use atlantis_core::{AtlantisError, Direction, Value};
use atlantis_cypher::ast::{
    Clause, Expression, NodePattern, PathPattern, Projection, Query, QueryUnion, RelPattern,
};

use super::pattern::pattern_variables;
use super::plan::{choose, describe, describe_access, pattern_hints, Cond, Operand, Pred};
use super::{Exec, QueryResult};

fn node_text(np: &NodePattern) -> String {
    let mut s = String::from("(");
    s.push_str(np.variable.as_deref().unwrap_or(""));
    for l in &np.labels {
        s.push(':');
        s.push_str(l);
    }
    s.push(')');
    s
}

fn rel_text(rp: &RelPattern) -> String {
    let mut inner = rp.variable.clone().unwrap_or_default();
    if !rp.types.is_empty() {
        inner.push(':');
        inner.push_str(&rp.types.join("|"));
    }
    if rp.length.is_some() {
        inner.push('*');
    }
    match rp.direction {
        Direction::Outgoing => format!("-[{inner}]->"),
        Direction::Incoming => format!("<-[{inner}]-"),
        Direction::Both => format!("-[{inner}]-"),
    }
}

struct Plan {
    rows: Vec<Vec<Value>>,
    bound: HashSet<String>,
}

impl Plan {
    fn op(&mut self, operator: &str, details: impl Into<String>) {
        self.rows.push(vec![
            Value::String(operator.into()),
            Value::String(details.into()),
        ]);
    }
}

impl Exec<'_> {
    pub(crate) fn explain(&mut self, union: &QueryUnion) -> Result<QueryResult, AtlantisError> {
        let mut plan = Plan {
            rows: Vec::new(),
            bound: HashSet::new(),
        };
        self.explain_query(&union.first, &mut plan)?;
        for (all, query) in &union.rest {
            plan.op(if *all { "UnionAll" } else { "Union" }, "");
            plan.bound.clear();
            self.explain_query(query, &mut plan)?;
        }
        Ok(QueryResult {
            columns: vec!["operator".into(), "details".into()],
            rows: plan.rows,
        })
    }

    fn explain_query(&mut self, query: &Query, plan: &mut Plan) -> Result<(), AtlantisError> {
        for clause in &query.clauses {
            match clause {
                Clause::Match {
                    optional,
                    pattern,
                    where_clause,
                } => {
                    if *optional {
                        plan.op("Optional", "");
                    }
                    let new_vars: Vec<String> = pattern_variables(pattern)
                        .into_iter()
                        .filter(|v| !plan.bound.contains(v))
                        .collect();
                    let hints = where_clause
                        .as_ref()
                        .map(|w| pattern_hints(w, pattern, |v| plan.bound.contains(v)))
                        .unwrap_or_default();
                    for path in &pattern.paths {
                        self.explain_path(path, &hints, plan)?;
                        // Later paths see this path's variables bound.
                        plan.bound.extend(path.start.variable.clone());
                        for (r, n) in &path.steps {
                            plan.bound.extend(r.variable.clone());
                            plan.bound.extend(n.variable.clone());
                        }
                    }
                    if let Some(w) = where_clause {
                        plan.op("Filter", describe(w));
                    }
                    plan.bound.extend(new_vars);
                }
                Clause::Create(pattern) => {
                    let text: Vec<String> = pattern.paths.iter().map(path_text).collect();
                    plan.op("Create", text.join(", "));
                    plan.bound.extend(pattern_variables(pattern));
                }
                Clause::Merge { path, .. } => {
                    plan.op("Merge", path_text(path));
                    plan.bound.extend(path.variable.clone());
                    plan.bound.extend(path.start.variable.clone());
                    for (r, n) in &path.steps {
                        plan.bound.extend(r.variable.clone());
                        plan.bound.extend(n.variable.clone());
                    }
                }
                Clause::Set(items) => plan.op("SetProperties", format!("{} item(s)", items.len())),
                Clause::Remove(items) => plan.op("Remove", format!("{} item(s)", items.len())),
                Clause::Delete { detach, exprs } => {
                    let text: Vec<String> = exprs.iter().map(describe).collect();
                    plan.op(
                        if *detach { "DetachDelete" } else { "Delete" },
                        text.join(", "),
                    );
                }
                Clause::With {
                    projection,
                    where_clause,
                } => {
                    explain_projection(projection, plan);
                    if let Some(w) = where_clause {
                        plan.op("Filter", describe(w));
                    }
                    let names: Vec<String> = projection
                        .items
                        .iter()
                        .map(|i| i.name().to_string())
                        .collect();
                    if !projection.star {
                        plan.bound.clear();
                    }
                    plan.bound.extend(names);
                }
                Clause::Unwind { expr, variable } => {
                    plan.op("Unwind", format!("{} AS {variable}", describe(expr)));
                    plan.bound.insert(variable.clone());
                }
                Clause::Call {
                    procedure, yields, ..
                } => {
                    plan.op("ProcedureCall", procedure.clone());
                    for y in yields.iter().flatten() {
                        plan.bound
                            .insert(y.alias.clone().unwrap_or_else(|| y.column.clone()));
                    }
                }
            }
        }
        if let Some(ret) = &query.ret {
            explain_projection(ret, plan);
        }
        Ok(())
    }

    fn explain_path(
        &mut self,
        path: &PathPattern,
        hints: &super::plan::Hints,
        plan: &mut Plan,
    ) -> Result<(), AtlantisError> {
        let np = &path.start;
        let var = np.variable.clone().unwrap_or_else(|| "anon".into());
        let bound = np.variable.as_ref().is_some_and(|v| plan.bound.contains(v));
        if bound {
            plan.op("Argument", var.clone());
        } else {
            let mut conds: Vec<Cond> = np
                .properties
                .iter()
                .map(|(k, e)| Cond {
                    path: vec![k.clone()],
                    pred: Pred::Eq(Operand::Expr(e.clone())),
                })
                .collect();
            if let Some(h) = np.variable.as_ref().and_then(|v| hints.get(v)) {
                conds.extend(h.iter().cloned());
            }
            let by_id = conds
                .iter()
                .find(|c| c.path == ["id"] && matches!(c.pred, Pred::Eq(_)));
            if let Some(Cond {
                pred: Pred::Eq(op), ..
            }) = by_id
            {
                let value = match op {
                    Operand::Expr(e) => describe(e),
                    Operand::Value(v) => v.to_string(),
                };
                plan.op(
                    "NodeByIdSeek",
                    format!("{} WHERE id = {value}", node_text(np)),
                );
            } else {
                let indexes = self.label_indexes(&np.labels)?;
                let access = choose(&np.labels, &conds, &indexes);
                let (operator, details) = describe_access(&var, &access);
                plan.op(&operator, details);
            }
        }
        let mut prev = np;
        for (rel, node) in &path.steps {
            let operator = if rel.length.is_some() {
                "VarLengthExpand(All)"
            } else {
                "Expand(All)"
            };
            plan.op(
                operator,
                format!("{}{}{}", node_text(prev), rel_text(rel), node_text(node)),
            );
            prev = node;
        }
        Ok(())
    }
}

fn path_text(path: &PathPattern) -> String {
    let mut s = node_text(&path.start);
    for (rel, node) in &path.steps {
        s.push_str(&rel_text(rel));
        s.push_str(&node_text(node));
    }
    s
}

fn explain_projection(p: &Projection, plan: &mut Plan) {
    let aggregates = p.items.iter().any(|i| i.expr.contains_aggregate());
    let items: Vec<&str> = p.items.iter().map(|i| i.name()).collect();
    let mut text = items.join(", ");
    if p.star {
        text = if text.is_empty() {
            "*".into()
        } else {
            format!("*, {text}")
        };
    }
    plan.op(
        if aggregates {
            "EagerAggregation"
        } else {
            "Projection"
        },
        text,
    );
    if p.distinct {
        plan.op("Distinct", "");
    }
    if !p.order_by.is_empty() {
        let keys: Vec<String> = p
            .order_by
            .iter()
            .map(|o| {
                let d: &Expression = &o.expr;
                format!("{}{}", describe(d), if o.descending { " DESC" } else { "" })
            })
            .collect();
        plan.op("Sort", keys.join(", "));
    }
    if let Some(s) = &p.skip {
        plan.op("Skip", describe(s));
    }
    if let Some(l) = &p.limit {
        plan.op("Limit", describe(l));
    }
}
