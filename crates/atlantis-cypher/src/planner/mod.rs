//! Cypher query planner.
//!
//! Converts the parsed AST into a logical plan of operators that the
//! executor can evaluate against the graph store.

pub mod logical;
pub mod optimizer;

pub use logical::LogicalOperator;
pub use optimizer::optimize;

use atlantis_core::{AtlantisError, Label, NodeId, RelType};
use atlantis_core::Direction;

use crate::parser::ast::*;

/// Plans a parsed Cypher statement into a logical operator tree.
pub fn plan(statement: &Statement) -> Result<LogicalOperator, AtlantisError> {
    match statement {
        Statement::Query(query) => plan_query(query),
    }
}

fn plan_query(query: &Query) -> Result<LogicalOperator, AtlantisError> {
    let mut plan: Option<LogicalOperator> = None;

    for clause in &query.clauses {
        match clause {
            Clause::Match(m) => {
                let match_plan = plan_match(m, plan.take())?;
                plan = Some(match_plan);
            }
            Clause::Where(expr) => {
                let input = plan.take().ok_or_else(|| {
                    AtlantisError::InvalidQuery("WHERE without preceding clause".into())
                })?;
                plan = Some(LogicalOperator::Filter {
                    input: Box::new(input),
                    predicate: expr.clone(),
                });
            }
            Clause::Create(c) => {
                let create_plan = plan_create(c, plan.take())?;
                plan = Some(create_plan);
            }
            Clause::Set(s) => {
                let input = plan.take().ok_or_else(|| {
                    AtlantisError::InvalidQuery("SET without preceding clause".into())
                })?;
                let mut ops: Vec<LogicalOperator> = Vec::new();
                for assignment in &s.assignments {
                    ops.push(LogicalOperator::SetProperty {
                        input: Box::new(plan.clone().unwrap_or(LogicalOperator::Empty)),
                        target: assignment.target.clone(),
                        key: property_key(&assignment.target).unwrap_or_default(),
                        value: assignment.value.clone(),
                    });
                }
                plan = Some(LogicalOperator::Sequence(ops));
            }
            Clause::Delete(d) => {
                let input = plan.take().ok_or_else(|| {
                    AtlantisError::InvalidQuery("DELETE without preceding clause".into())
                })?;
                plan = Some(LogicalOperator::Delete {
                    input: Box::new(input),
                    targets: d.expressions.clone(),
                    detach: d.detach,
                });
            }
            Clause::Call(call) => {
                let call_plan = plan_call(call, plan.take())?;
                plan = Some(call_plan);
            }
            Clause::With(w) => {
                let input = plan.take().ok_or_else(|| {
                    AtlantisError::InvalidQuery("WITH without preceding clause".into())
                })?;
                plan = Some(LogicalOperator::Project {
                    input: Box::new(input),
                    items: w.items.iter().map(|item| {
                        (item.expression.clone(), item.alias.clone())
                    }).collect(),
                });
                if let Some(where_clause) = &w.where_clause {
                    plan = Some(LogicalOperator::Filter {
                        input: Box::new(plan.take().unwrap()),
                        predicate: where_clause.clone(),
                    });
                }
            }
            Clause::Unwind(u) => {
                let input = plan.take().unwrap_or(LogicalOperator::Empty);
                plan = Some(LogicalOperator::Unwind {
                    input: Box::new(input),
                    expression: u.expression.clone(),
                    variable: u.variable.clone(),
                });
            }
            _ => {}
        }
    }

    // Add RETURN as a final projection
    if let Some(ret) = &query.result {
        let input = plan.take().unwrap_or(LogicalOperator::Empty);
        plan = Some(LogicalOperator::Project {
            input: Box::new(input),
            items: ret.items.iter().map(|item| {
                (item.expression.clone(), item.alias.clone())
            }).collect(),
        });

        // Add ORDER BY
        if let Some(order_items) = &ret.order_by {
            plan = Some(LogicalOperator::Sort {
                input: Box::new(plan.take().unwrap()),
                keys: order_items.iter().map(|oi| (oi.expression.clone(), oi.descending)).collect(),
            });
        }

        // Add SKIP
        if let Some(skip) = &ret.skip {
            plan = Some(LogicalOperator::Skip {
                input: Box::new(plan.take().unwrap()),
                count: skip.clone(),
            });
        }

        // Add LIMIT
        if let Some(limit) = &ret.limit {
            plan = Some(LogicalOperator::Limit {
                input: Box::new(plan.take().unwrap()),
                count: limit.clone(),
            });
        }
    }

    plan.ok_or_else(|| AtlantisError::InvalidQuery("empty query".into()))
}

fn plan_match(m: &MatchClause, input: Option<LogicalOperator>) -> Result<LogicalOperator, AtlantisError> {
    let mut current = input;

    for element in &m.pattern.elements {
        match element {
            PatternElement::Node(np) => {
                let var = np.variable.clone().unwrap_or_else(|| "_anon".into());
                if np.labels.is_empty() {
                    current = Some(LogicalOperator::AllNodesScan {
                        var,
                        input: current.map(Box::new),
                    });
                } else {
                    // Use the first label for scanning
                    let label = np.labels[0].clone();
                    current = Some(LogicalOperator::NodeByLabelScan {
                        var,
                        label,
                        input: current.map(Box::new),
                    });
                }
            }
            PatternElement::Relationship(rp) => {
                let from_var = current
                    .as_ref()
                    .map(|c| get_last_node_var(c))
                    .unwrap_or_else(|| "_start".to_string());
                let to_var = "_next".to_string();
                let rel_var = rp.variable.clone().unwrap_or_else(|| "_rel".into());
                let rel_type = rp.rel_type.clone();
                let direction = rp.direction;

                current = Some(LogicalOperator::Expand {
                    from: from_var,
                    to: to_var,
                    rel_var,
                    rel_type,
                    direction,
                    input: Box::new(current.unwrap_or(LogicalOperator::Empty)),
                });
            }
        }
    }

    Ok(current.unwrap_or(LogicalOperator::Empty))
}

fn plan_create(c: &CreateClause, _input: Option<LogicalOperator>) -> Result<LogicalOperator, AtlantisError> {
    let mut ops = Vec::new();

    for element in &c.pattern.elements {
        if let PatternElement::Node(np) = element {
            let var = np.variable.clone().unwrap_or_else(|| "_created".into());
            let labels: Vec<Label> = np.labels.clone();
            let properties: Vec<(String, Expression)> = np.properties.clone();
            ops.push(LogicalOperator::CreateNode {
                var,
                labels,
                properties,
            });
        }
    }

    Ok(LogicalOperator::Sequence(ops))
}

fn plan_call(call: &CallClause, _input: Option<LogicalOperator>) -> Result<LogicalOperator, AtlantisError> {
    if call.procedure == "db.vector.search" {
        let label = if let Some(Expression::Literal(atlantis_core::Value::String(s))) = call.arguments.first() {
            Label::from(s.as_str())
        } else {
            return Err(AtlantisError::InvalidQuery(
                "db.vector.search requires a label string as first argument".into(),
            ));
        };

        let query = call.arguments.get(1).cloned().ok_or_else(|| {
            AtlantisError::InvalidQuery("db.vector.search requires a query vector".into())
        })?;

        let k = match call.arguments.get(2) {
            Some(Expression::Literal(atlantis_core::Value::Integer(n))) => *n as usize,
            _ => return Err(AtlantisError::InvalidQuery(
                "db.vector.search requires an integer k as third argument".into(),
            )),
        };

        let threshold = match call.arguments.get(3) {
            Some(Expression::Literal(atlantis_core::Value::Float(f))) => Some(*f as f32),
            Some(Expression::Literal(atlantis_core::Value::Integer(i))) => Some(*i as f32),
            _ => None,
        };

        let yield_items = call.yield_items.as_ref();
        let node_var = yield_items
            .and_then(|items| items.first())
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "node".into());
        let score_var = yield_items
            .and_then(|items| items.get(1))
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| "score".into());

        Ok(LogicalOperator::VectorSearch {
            label,
            query,
            k,
            threshold,
            node_var,
            score_var,
        })
    } else {
        Err(AtlantisError::InvalidQuery(format!(
            "unknown procedure: {}",
            call.procedure
        )))
    }
}

fn get_last_node_var(plan: &LogicalOperator) -> String {
    match plan {
        LogicalOperator::NodeByLabelScan { var, .. } => var.clone(),
        LogicalOperator::AllNodesScan { var, .. } => var.clone(),
        LogicalOperator::Expand { to, .. } => to.clone(),
        LogicalOperator::VectorSearch { node_var, .. } => node_var.clone(),
        _ => "_start".to_string(),
    }
}

fn property_key(expr: &Expression) -> Option<String> {
    match expr {
        Expression::Property(_, key) => Some(key.clone()),
        _ => None,
    }
}
