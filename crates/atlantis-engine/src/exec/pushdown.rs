//! Filter push-down for `CALL db.vector.search(...) YIELD ... WHERE ...`:
//! picks the `WHERE` conjuncts the vector index can evaluate itself.

use atlantis_cypher::ast::{BinaryOp, Expression, YieldItem};

use super::eval::{is_comparison, lowercase};

/// A `WHERE` condition the vector search can apply inside the index:
/// `node.field op value`, where `value` does not depend on the yielded row.
pub(super) struct PushedCondition<'q> {
    pub(super) field: String,
    pub(super) op: BinaryOp,
    pub(super) value: &'q Expression,
    pub(super) field_on_left: bool,
}

/// The conjuncts of `where_clause` that `db.vector.search` can evaluate
/// itself: comparisons between a top-level field of the yielded `node` and
/// an expression that mentions no yielded column and is deterministic.
pub(super) fn pushable_conditions<'q>(
    where_clause: &'q Expression,
    yields: Option<&Vec<YieldItem>>,
) -> Vec<PushedCondition<'q>> {
    let yielded: Vec<String> = match yields {
        None => vec!["node".into(), "score".into()],
        Some(items) => items
            .iter()
            .map(|y| y.alias.clone().unwrap_or_else(|| y.column.clone()))
            .collect(),
    };
    let node = match yields {
        None => Some("node".to_string()),
        Some(items) => items
            .iter()
            .find(|y| y.column == "node")
            .map(|y| y.alias.clone().unwrap_or_else(|| y.column.clone())),
    };
    let Some(node) = node else {
        return Vec::new();
    };
    let field_of = |e: &Expression| match e {
        Expression::Property(base, key) if key != "id" => match base.as_ref() {
            Expression::Variable(v) if *v == node => Some(key.clone()),
            _ => None,
        },
        _ => None,
    };
    let constant = |e: &Expression| !mentions_any(e, &yielded) && deterministic(e);
    let mut conjuncts = Vec::new();
    split_and(where_clause, &mut conjuncts);
    conjuncts
        .into_iter()
        .filter_map(|c| match c {
            Expression::Binary(l, op, r) if is_comparison(*op) => {
                if let (Some(field), true) = (field_of(l), constant(r)) {
                    Some(PushedCondition {
                        field,
                        op: *op,
                        value: r,
                        field_on_left: true,
                    })
                } else if let (Some(field), true) = (field_of(r), constant(l)) {
                    Some(PushedCondition {
                        field,
                        op: *op,
                        value: l,
                        field_on_left: false,
                    })
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect()
}

fn split_and<'q>(e: &'q Expression, out: &mut Vec<&'q Expression>) {
    match e {
        Expression::Binary(l, BinaryOp::And, r) => {
            split_and(l, out);
            split_and(r, out);
        }
        other => out.push(other),
    }
}

/// `true` if `e` refers to any of `names` (conservatively: any variable of
/// that name, even one a list comprehension rebinds).
fn mentions_any(e: &Expression, names: &[String]) -> bool {
    match e {
        Expression::Variable(v) => names.contains(v),
        Expression::Literal(_) | Expression::Parameter(_) | Expression::CountStar => false,
        Expression::Property(b, _) | Expression::HasLabels(b, _) | Expression::Unary(_, b) => {
            mentions_any(b, names)
        }
        Expression::IsNull(b, _) => mentions_any(b, names),
        Expression::Index(a, b) | Expression::Binary(a, _, b) => {
            mentions_any(a, names) || mentions_any(b, names)
        }
        Expression::Slice(a, b, c) => {
            mentions_any(a, names)
                || b.as_deref().is_some_and(|x| mentions_any(x, names))
                || c.as_deref().is_some_and(|x| mentions_any(x, names))
        }
        Expression::List(items) | Expression::Function { args: items, .. } => {
            items.iter().any(|x| mentions_any(x, names))
        }
        Expression::Map(entries) => entries.iter().any(|(_, v)| mentions_any(v, names)),
        Expression::Case {
            operand,
            whens,
            default,
        } => {
            operand.as_deref().is_some_and(|x| mentions_any(x, names))
                || whens
                    .iter()
                    .any(|(a, b)| mentions_any(a, names) || mentions_any(b, names))
                || default.as_deref().is_some_and(|x| mentions_any(x, names))
        }
        Expression::ListComprehension {
            variable,
            list,
            predicate,
            map,
        } => {
            names.contains(variable)
                || mentions_any(list, names)
                || predicate.as_deref().is_some_and(|x| mentions_any(x, names))
                || map.as_deref().is_some_and(|x| mentions_any(x, names))
        }
    }
}

/// `false` if `e` calls a function whose result changes between calls, so
/// evaluating it once for the push-down could disagree with `WHERE`.
fn deterministic(e: &Expression) -> bool {
    match e {
        Expression::Function { name, args, .. } => {
            let n = lowercase(name);
            let volatile = matches!(n.as_ref(), "rand" | "randomuuid" | "timestamp")
                || (args.is_empty()
                    && matches!(n.as_ref(), "datetime" | "date" | "time" | "localdatetime"));
            !volatile && args.iter().all(deterministic)
        }
        Expression::Property(b, _)
        | Expression::HasLabels(b, _)
        | Expression::Unary(_, b)
        | Expression::IsNull(b, _) => deterministic(b),
        Expression::Index(a, b) | Expression::Binary(a, _, b) => {
            deterministic(a) && deterministic(b)
        }
        Expression::Slice(a, b, c) => {
            deterministic(a)
                && b.as_deref().is_none_or(deterministic)
                && c.as_deref().is_none_or(deterministic)
        }
        Expression::List(items) => items.iter().all(deterministic),
        Expression::Map(entries) => entries.iter().all(|(_, v)| deterministic(v)),
        Expression::Case {
            operand,
            whens,
            default,
        } => {
            operand.as_deref().is_none_or(deterministic)
                && whens
                    .iter()
                    .all(|(a, b)| deterministic(a) && deterministic(b))
                && default.as_deref().is_none_or(deterministic)
        }
        Expression::ListComprehension {
            list,
            predicate,
            map,
            ..
        } => {
            deterministic(list)
                && predicate.as_deref().is_none_or(deterministic)
                && map.as_deref().is_none_or(deterministic)
        }
        Expression::Literal(_)
        | Expression::Parameter(_)
        | Expression::Variable(_)
        | Expression::CountStar => true,
    }
}
