//! Access paths for the start node of a pattern: by id, through a property
//! index, by label, or over all nodes.
//!
//! Conditions come from the node's inline properties (`(p:Person {email:
//! $e})`) and from `WHERE` conjuncts of the form `p.path op value`, where
//! `value` mentions no variable the pattern binds and is deterministic.
//! The planner picks the most selective usable index; the executor then
//! looks up candidates and still runs every check (labels, inline
//! properties, `WHERE`) on each of them, so an index only ever narrows the
//! work, never the answer.

use std::collections::{HashMap, HashSet};

use atlantis_core::{AtlantisError, RecordId, Value};
use atlantis_cypher::ast::{BinaryOp, Expression, NodePattern, PathPattern, Pattern};
use atlantis_storage::{display_path, IndexInfo, IndexLookup};

use super::pushdown::{deterministic, mentions_any, split_and};
use super::{Exec, Row};

/// A value a condition compares against: an expression evaluated per input
/// row, or a value already evaluated (inline pattern properties).
#[derive(Debug, Clone)]
pub(crate) enum Operand {
    Expr(Expression),
    Value(Value),
}

/// A condition on one property path of a node.
#[derive(Debug, Clone)]
pub(crate) enum Pred {
    /// `n.path = v`
    Eq(Operand),
    /// `n.path IN list`
    In(Operand),
    /// `v IN n.path` (answered by multikey indexes)
    Contains(Operand),
    /// `n.path > v` / `n.path >= v`
    Lower(Operand),
    /// `n.path < v` / `n.path <= v`
    Upper(Operand),
    /// `n.path STARTS WITH v`
    Prefix(Operand),
}

#[derive(Debug, Clone)]
pub(crate) struct Cond {
    pub(crate) path: Vec<String>,
    pub(crate) pred: Pred,
}

/// Conditions per variable, from a `WHERE` clause.
pub(crate) type Hints = HashMap<String, Vec<Cond>>;

/// How the start node of a path is found.
#[derive(Debug, Clone)]
pub(crate) enum Access {
    Index { index: Box<IndexInfo>, seek: Seek },
    LabelScan(String),
    AllNodes,
}

/// What an index lookup asks for.
#[derive(Debug, Clone)]
pub(crate) enum Seek {
    /// Equality on the leading fields.
    Eq(Vec<Operand>),
    /// The first field is one of a list.
    AnyOf(Operand),
    /// The first field (an array, multikey) contains a value.
    Contains(Operand),
    Range {
        lower: Option<Operand>,
        upper: Option<Operand>,
    },
    Prefix(Operand),
}

/// `node.path` if `e` is a property chain on a variable in `vars`.
fn path_of(e: &Expression, vars: &[String]) -> Option<(String, Vec<String>)> {
    let mut path = Vec::new();
    let mut cur = e;
    loop {
        match cur {
            Expression::Property(base, key) => {
                path.push(key.clone());
                cur = base;
            }
            Expression::Variable(v) if !path.is_empty() && vars.contains(v) => {
                path.reverse();
                return Some((v.clone(), path));
            }
            _ => return None,
        }
    }
}

/// Index-usable conditions in `where_clause` on the variables `vars` (the
/// variables a pattern binds).
fn where_conditions(where_clause: &Expression, vars: &[String]) -> Hints {
    let mut conjuncts = Vec::new();
    split_and(where_clause, &mut conjuncts);
    let constant = |e: &Expression| !mentions_any(e, vars) && deterministic(e);
    let mut hints: Hints = HashMap::new();
    for c in conjuncts {
        let Expression::Binary(l, op, r) = c else {
            continue;
        };
        let (l, r) = (l.as_ref(), r.as_ref());
        let found = match (path_of(l, vars), path_of(r, vars)) {
            (Some(p), _) if constant(r) => {
                let v = Operand::Expr(r.clone());
                match op {
                    BinaryOp::Eq => Some((p, Pred::Eq(v))),
                    BinaryOp::In => Some((p, Pred::In(v))),
                    BinaryOp::Gt | BinaryOp::GtEq => Some((p, Pred::Lower(v))),
                    BinaryOp::Lt | BinaryOp::LtEq => Some((p, Pred::Upper(v))),
                    BinaryOp::StartsWith => Some((p, Pred::Prefix(v))),
                    _ => None,
                }
            }
            (_, Some(p)) if constant(l) => {
                let v = Operand::Expr(l.clone());
                match op {
                    BinaryOp::Eq => Some((p, Pred::Eq(v))),
                    BinaryOp::In => Some((p, Pred::Contains(v))),
                    BinaryOp::Gt | BinaryOp::GtEq => Some((p, Pred::Upper(v))),
                    BinaryOp::Lt | BinaryOp::LtEq => Some((p, Pred::Lower(v))),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(((var, path), pred)) = found {
            hints.entry(var).or_default().push(Cond { path, pred });
        }
    }
    hints
}

/// Variables a path binds.
fn path_vars(path: &PathPattern) -> impl Iterator<Item = &String> {
    path.variable
        .iter()
        .chain(path.start.variable.iter())
        .chain(
            path.steps
                .iter()
                .flat_map(|(r, n)| r.variable.iter().chain(n.variable.iter())),
        )
}

/// Index-usable conditions for the start node of each path of `pattern`.
/// Paths are matched in order, so the conditions for a path's start node
/// may use variables bound by earlier paths (an index nested loop join:
/// `MATCH (o:Order), (c:Customer) WHERE c.email = o.email`), but not
/// variables of that path or later ones. `bound` tells which variables
/// are already bound before the pattern.
pub(crate) fn pattern_hints(
    where_clause: &Expression,
    pattern: &Pattern,
    bound: impl Fn(&str) -> bool,
) -> Hints {
    let mut hints = Hints::new();
    for (i, path) in pattern.paths.iter().enumerate() {
        let Some(start) = &path.start.variable else {
            continue;
        };
        if bound(start) || hints.contains_key(start) {
            continue;
        }
        let unbound: Vec<String> = pattern.paths[i..]
            .iter()
            .flat_map(path_vars)
            .filter(|v| !bound(v))
            .cloned()
            .collect();
        if let Some(conds) = where_conditions(where_clause, &unbound).remove(start) {
            hints.insert(start.clone(), conds);
        }
    }
    hints
}

/// Picks an access path for a node with `labels`, given its conditions
/// and the indexes on each label. Higher scores are more selective.
pub(crate) fn choose(labels: &[String], conds: &[Cond], indexes: &[Vec<IndexInfo>]) -> Access {
    fn on<'a>(conds: &'a [Cond], path: &'a [String]) -> impl Iterator<Item = &'a Cond> + 'a {
        conds.iter().filter(move |c| c.path == path)
    }
    let mut best: Option<(u32, IndexInfo, Seek)> = None;
    for index in indexes.iter().flatten() {
        // A composite index has no entry for a node missing any field, so
        // it can answer only when every field is constrained (any condition
        // here is false for null) or every node has an entry.
        let covered = index.complete || index.fields.iter().all(|f| on(conds, f).next().is_some());
        if index.fields.len() > 1 && !covered {
            continue;
        }
        let mut eq = Vec::new();
        for field in &index.fields {
            match on(conds, field).find_map(|c| match &c.pred {
                Pred::Eq(v) => Some(v.clone()),
                _ => None,
            }) {
                Some(v) => eq.push(v),
                None => break,
            }
        }
        let first = &index.fields[0];
        let first_pred = |want: fn(&Pred) -> Option<&Operand>| {
            on(conds, first).find_map(|c| want(&c.pred).cloned())
        };
        let candidate = if eq.len() == index.fields.len() {
            let score = if index.unique {
                100
            } else {
                80 + eq.len() as u32
            };
            Some((score, Seek::Eq(eq)))
        } else if !eq.is_empty() {
            Some((60 + eq.len() as u32, Seek::Eq(eq)))
        } else if let Some(v) = first_pred(|p| match p {
            Pred::In(v) => Some(v),
            _ => None,
        }) {
            Some((50, Seek::AnyOf(v)))
        } else if let Some(v) = first_pred(|p| match p {
            Pred::Contains(v) => Some(v),
            _ => None,
        })
        .filter(|_| index.multikey)
        {
            Some((45, Seek::Contains(v)))
        } else if let Some(v) = first_pred(|p| match p {
            Pred::Prefix(v) => Some(v),
            _ => None,
        }) {
            Some((35, Seek::Prefix(v)))
        } else {
            let lower = first_pred(|p| match p {
                Pred::Lower(v) => Some(v),
                _ => None,
            });
            let upper = first_pred(|p| match p {
                Pred::Upper(v) => Some(v),
                _ => None,
            });
            (lower.is_some() || upper.is_some()).then_some((30, Seek::Range { lower, upper }))
        };
        if let Some((score, seek)) = candidate {
            let better = best
                .as_ref()
                .is_none_or(|(s, i, _)| score > *s || (score == *s && index.unique && !i.unique));
            if better {
                best = Some((score, index.clone(), seek));
            }
        }
    }
    match (best, labels.first()) {
        (Some((_, index, seek)), _) => Access::Index {
            index: Box::new(index),
            seek,
        },
        (None, Some(label)) => Access::LabelScan(label.clone()),
        (None, None) => Access::AllNodes,
    }
}

impl Exec<'_> {
    /// The conditions on node pattern `np`: its evaluated inline properties
    /// and the `WHERE` hints of the current `MATCH`.
    pub(crate) fn node_conditions(&self, np: &NodePattern, props: &[(String, Value)]) -> Vec<Cond> {
        let mut conds: Vec<Cond> = props
            .iter()
            .filter(|(k, _)| k != "id")
            .map(|(k, v)| Cond {
                path: vec![k.clone()],
                pred: Pred::Eq(Operand::Value(v.clone())),
            })
            .collect();
        if let Some(hints) = np.variable.as_ref().and_then(|v| self.hints.get(v)) {
            conds.extend(hints.iter().cloned());
        }
        conds
    }

    /// The indexes on each of `labels`.
    pub(crate) fn label_indexes(
        &mut self,
        labels: &[String],
    ) -> Result<Vec<Vec<IndexInfo>>, AtlantisError> {
        labels.iter().map(|l| self.tx.indexes_for(l)).collect()
    }

    /// Evaluates an operand; `None` if evaluation fails, in which case the
    /// caller falls back to a scan and `WHERE` reports the error itself.
    fn operand(&mut self, op: &Operand, row: &Row) -> Option<Value> {
        match op {
            Operand::Value(v) => Some(v.clone()),
            Operand::Expr(e) => self.eval(e, row).ok(),
        }
    }

    /// Candidate records from an index, or `None` to fall back to a scan.
    pub(crate) fn index_candidates(
        &mut self,
        index: &IndexInfo,
        seek: &Seek,
        row: &Row,
    ) -> Result<Option<Vec<RecordId>>, AtlantisError> {
        let lookups: Vec<IndexLookup> = match seek {
            Seek::Eq(ops) => {
                let mut values = Vec::with_capacity(ops.len());
                for op in ops {
                    let Some(v) = self.operand(op, row) else {
                        return Ok(None);
                    };
                    values.push(v);
                }
                vec![IndexLookup::Eq(values)]
            }
            Seek::Contains(op) => {
                let Some(v) = self.operand(op, row) else {
                    return Ok(None);
                };
                vec![IndexLookup::Eq(vec![v])]
            }
            Seek::AnyOf(op) => match self.operand(op, row) {
                Some(Value::Null) => return Ok(Some(Vec::new())),
                Some(Value::Array(items)) => items
                    .into_iter()
                    .filter(|v| !v.is_null())
                    .map(|v| IndexLookup::Eq(vec![v]))
                    .collect(),
                _ => return Ok(None),
            },
            Seek::Prefix(op) => match self.operand(op, row) {
                Some(Value::String(s)) => vec![IndexLookup::Prefix(s)],
                Some(Value::Null) => return Ok(Some(Vec::new())),
                _ => return Ok(None),
            },
            Seek::Range { lower, upper } => {
                let mut bound = |op: &Option<Operand>| -> Option<Option<Value>> {
                    match op {
                        None => Some(None),
                        Some(op) => self.operand(op, row).map(Some),
                    }
                };
                let (Some(lower), Some(upper)) = (bound(lower), bound(upper)) else {
                    return Ok(None);
                };
                vec![IndexLookup::Range { lower, upper }]
            }
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for lookup in &lookups {
            let Some(ids) = self.tx.index_lookup(index, lookup)? else {
                return Ok(None);
            };
            for id in ids {
                if seen.insert(id.to_string()) {
                    out.push(id);
                }
            }
        }
        Ok(Some(out))
    }
}

/// A short description of an expression for `EXPLAIN`.
pub(crate) fn describe(e: &Expression) -> String {
    match e {
        Expression::Variable(v) => v.clone(),
        Expression::Parameter(p) => format!("${p}"),
        Expression::Literal(v) => v.to_string(),
        Expression::Property(base, key) => format!("{}.{key}", describe(base)),
        Expression::List(items) => {
            let items: Vec<String> = items.iter().map(describe).collect();
            format!("[{}]", items.join(", "))
        }
        Expression::Function { name, args, .. } => {
            let args: Vec<String> = args.iter().map(describe).collect();
            format!("{name}({})", args.join(", "))
        }
        Expression::Binary(l, op, r) => {
            let sym = match op {
                BinaryOp::Or => "OR",
                BinaryOp::Xor => "XOR",
                BinaryOp::And => "AND",
                BinaryOp::Eq => "=",
                BinaryOp::NotEq => "<>",
                BinaryOp::Lt => "<",
                BinaryOp::LtEq => "<=",
                BinaryOp::Gt => ">",
                BinaryOp::GtEq => ">=",
                BinaryOp::RegexMatch => "=~",
                BinaryOp::In => "IN",
                BinaryOp::StartsWith => "STARTS WITH",
                BinaryOp::EndsWith => "ENDS WITH",
                BinaryOp::Contains => "CONTAINS",
                BinaryOp::Add => "+",
                BinaryOp::Sub => "-",
                BinaryOp::Mul => "*",
                BinaryOp::Div => "/",
                BinaryOp::Mod => "%",
                BinaryOp::Pow => "^",
            };
            format!("{} {sym} {}", describe(l), describe(r))
        }
        Expression::IsNull(base, negated) => {
            format!(
                "{} IS {}NULL",
                describe(base),
                if *negated { "NOT " } else { "" }
            )
        }
        _ => "…".into(),
    }
}

fn describe_operand(op: &Operand) -> String {
    match op {
        Operand::Expr(e) => describe(e),
        Operand::Value(v) => v.to_string(),
    }
}

/// `EXPLAIN` text for an access path of variable `var`.
pub(crate) fn describe_access(var: &str, access: &Access) -> (String, String) {
    match access {
        Access::Index { index, seek } => {
            let label = &index.label;
            let field = |i: usize| format!("{var}.{}", display_path(&index.fields[i]));
            let (operator, cond) = match seek {
                Seek::Eq(ops) => {
                    let parts: Vec<String> = ops
                        .iter()
                        .enumerate()
                        .map(|(i, op)| format!("{} = {}", field(i), describe_operand(op)))
                        .collect();
                    let op = if index.unique && ops.len() == index.fields.len() {
                        "NodeUniqueIndexSeek"
                    } else {
                        "NodeIndexSeek"
                    };
                    (op, parts.join(" AND "))
                }
                Seek::AnyOf(op) => (
                    "NodeIndexSeek",
                    format!("{} IN {}", field(0), describe_operand(op)),
                ),
                Seek::Contains(op) => (
                    "NodeIndexSeek",
                    format!("{} IN {}", describe_operand(op), field(0)),
                ),
                Seek::Prefix(op) => (
                    "NodeIndexSeekByRange",
                    format!("{} STARTS WITH {}", field(0), describe_operand(op)),
                ),
                Seek::Range { lower, upper } => {
                    let mut parts = Vec::new();
                    if let Some(l) = lower {
                        parts.push(format!("{} >= {}", field(0), describe_operand(l)));
                    }
                    if let Some(u) = upper {
                        parts.push(format!("{} <= {}", field(0), describe_operand(u)));
                    }
                    ("NodeIndexSeekByRange", parts.join(" AND "))
                }
            };
            (
                operator.into(),
                format!("{var}:{label} WHERE {cond} (index {})", index.name),
            )
        }
        Access::LabelScan(label) => ("NodeByLabelScan".into(), format!("{var}:{label}")),
        Access::AllNodes => ("AllNodesScan".into(), var.to_string()),
    }
}
