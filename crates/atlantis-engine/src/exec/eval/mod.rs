//! Expression evaluation.
//!
//! Semantics follow Cypher: `null` propagates through operators and
//! comparisons, boolean operators use three-valued logic, and property
//! access on a record id loads the record (so stored record links
//! dereference transparently).

mod access;
mod arith;
mod compare;
mod functions;

use atlantis_core::{AtlantisError, Object, Value};
use atlantis_cypher::ast::{BinaryOp, Expression, UnaryOp};

use super::{Exec, Row};
use arith::arithmetic;
pub(crate) use arith::plain;
use compare::cypher_eq;
pub(crate) use compare::{compare_op, group_key, is_comparison, order_cmp};
pub(crate) use functions::lowercase;

pub(crate) fn invalid(msg: impl Into<String>) -> AtlantisError {
    AtlantisError::InvalidQuery(msg.into())
}

/// Truthiness for `WHERE`: only `true` passes.
pub(crate) fn is_true(v: &Value) -> bool {
    matches!(v, Value::Bool(true))
}

fn as_bool(v: &Value) -> Result<Option<bool>, AtlantisError> {
    match v {
        Value::Null => Ok(None),
        Value::Bool(b) => Ok(Some(*b)),
        other => Err(invalid(format!(
            "expected a boolean, got {}",
            other.type_name()
        ))),
    }
}

fn int_arg(v: &Value, what: &str) -> Result<Option<i64>, AtlantisError> {
    match v {
        Value::Null => Ok(None),
        Value::Integer(i) => Ok(Some(*i)),
        Value::Float(f) if f.fract() == 0.0 => Ok(Some(*f as i64)),
        other => Err(invalid(format!(
            "{what} must be an integer, got {}",
            other.type_name()
        ))),
    }
}

pub(crate) fn as_vector(v: &Value) -> Option<Vec<f32>> {
    match v {
        Value::Vector(v) => Some(v.clone()),
        Value::Array(items) => items
            .iter()
            .map(|x| x.as_float().map(|f| f as f32))
            .collect(),
        _ => None,
    }
}

impl Exec<'_> {
    pub(crate) fn eval(&mut self, expr: &Expression, row: &Row) -> Result<Value, AtlantisError> {
        Ok(match expr {
            Expression::Literal(v) => v.clone(),
            Expression::Parameter(name) => self
                .params
                .get(name)
                .cloned()
                .ok_or_else(|| invalid(format!("missing parameter ${name}")))?,
            Expression::Variable(name) => row
                .get(name)
                .cloned()
                .ok_or_else(|| invalid(format!("variable `{name}` is not defined")))?,
            Expression::Property(base, key) => {
                let base = self.eval(base, row)?;
                self.property(base, key)?
            }
            Expression::Index(base, index) => {
                let (base, index) = (self.eval(base, row)?, self.eval(index, row)?);
                self.index(base, index)?
            }
            Expression::Slice(base, from, to) => {
                self.slice(base, from.as_deref(), to.as_deref(), row)?
            }
            Expression::HasLabels(base, labels) => match self.eval(base, row)? {
                Value::Null => Value::Null,
                Value::RecordId(id) => {
                    let have = self.labels_of(&id)?;
                    Value::Bool(labels.iter().all(|l| have.contains(l)))
                }
                other => return Err(invalid(format!("a {} has no labels", other.type_name()))),
            },
            Expression::List(items) => Value::Array(
                items
                    .iter()
                    .map(|e| self.eval(e, row))
                    .collect::<Result<_, _>>()?,
            ),
            Expression::Map(items) => {
                let mut obj = Object::new();
                for (k, e) in items {
                    obj.insert(k.clone(), self.eval(e, row)?);
                }
                Value::Object(obj)
            }
            Expression::Unary(op, e) => {
                let v = self.eval(e, row)?;
                match (op, v) {
                    (_, Value::Null) => Value::Null,
                    (UnaryOp::Not, v) => Value::Bool(!as_bool(&v)?.expect("non-null")),
                    (UnaryOp::Neg, Value::Integer(i)) => {
                        Value::Integer(i.checked_neg().ok_or_else(|| invalid("integer overflow"))?)
                    }
                    (UnaryOp::Neg, Value::Float(f)) => Value::Float(-f),
                    (UnaryOp::Neg, Value::Duration(d)) => {
                        Value::Duration(atlantis_core::Duration(-d.0))
                    }
                    (UnaryOp::Plus, v @ (Value::Integer(_) | Value::Float(_))) => v,
                    (op, v) => {
                        return Err(invalid(format!(
                            "cannot apply {op:?} to a {}",
                            v.type_name()
                        )))
                    }
                }
            }
            Expression::Binary(l, op, r) => self.binary(l, *op, r, row)?,
            Expression::IsNull(e, negated) => {
                let null = self.eval(e, row)?.is_null();
                Value::Bool(null != *negated)
            }
            Expression::Case {
                operand,
                whens,
                default,
            } => {
                let subject = operand.as_ref().map(|e| self.eval(e, row)).transpose()?;
                for (when, then) in whens {
                    let w = self.eval(when, row)?;
                    let hit = match &subject {
                        Some(s) => is_true(&cypher_eq(s, &w)),
                        None => is_true(&w),
                    };
                    if hit {
                        return self.eval(then, row);
                    }
                }
                match default {
                    Some(d) => self.eval(d, row)?,
                    None => Value::Null,
                }
            }
            Expression::ListComprehension {
                variable,
                list,
                predicate,
                map,
            } => {
                let items = match self.eval(list, row)? {
                    Value::Null => return Ok(Value::Null),
                    Value::Array(items) => items,
                    other => {
                        return Err(invalid(format!(
                            "expected a list, got {}",
                            other.type_name()
                        )))
                    }
                };
                let mut scope = row.clone();
                let mut out = Vec::new();
                for item in items {
                    // Every iteration rebinds the variable before reading it.
                    scope.insert(variable.clone(), item);
                    if let Some(p) = predicate {
                        if !is_true(&self.eval(p, &scope)?) {
                            continue;
                        }
                    }
                    out.push(match map {
                        Some(m) => self.eval(m, &scope)?,
                        None => scope.remove(variable).expect("bound above"),
                    });
                }
                Value::Array(out)
            }
            Expression::CountStar => {
                return Err(invalid("count(*) is only allowed in RETURN or WITH"))
            }
            Expression::Function {
                name,
                distinct: _,
                args,
            } => {
                if atlantis_cypher::ast::is_aggregate(name) {
                    return Err(invalid(format!(
                        "aggregate {name}() is only allowed in RETURN or WITH"
                    )));
                }
                let args: Vec<Value> = args
                    .iter()
                    .map(|a| self.eval(a, row))
                    .collect::<Result<_, _>>()?;
                self.call_function(name, args)?
            }
        })
    }

    fn binary(
        &mut self,
        l: &Expression,
        op: BinaryOp,
        r: &Expression,
        row: &Row,
    ) -> Result<Value, AtlantisError> {
        // Short-circuit boolean operators with three-valued logic.
        if matches!(op, BinaryOp::And | BinaryOp::Or) {
            let a = as_bool(&self.eval(l, row)?)?;
            if op == BinaryOp::And && a == Some(false) {
                return Ok(Value::Bool(false));
            }
            if op == BinaryOp::Or && a == Some(true) {
                return Ok(Value::Bool(true));
            }
            let b = as_bool(&self.eval(r, row)?)?;
            return Ok(match (op, a, b) {
                (BinaryOp::And, _, Some(false)) => Value::Bool(false),
                (BinaryOp::And, Some(true), Some(true)) => Value::Bool(true),
                (BinaryOp::Or, _, Some(true)) => Value::Bool(true),
                (BinaryOp::Or, Some(false), Some(false)) => Value::Bool(false),
                _ => Value::Null,
            });
        }
        let (a, b) = (self.eval(l, row)?, self.eval(r, row)?);
        Ok(match op {
            BinaryOp::Xor => match (as_bool(&a)?, as_bool(&b)?) {
                (Some(x), Some(y)) => Value::Bool(x != y),
                _ => Value::Null,
            },
            BinaryOp::Eq
            | BinaryOp::NotEq
            | BinaryOp::Lt
            | BinaryOp::LtEq
            | BinaryOp::Gt
            | BinaryOp::GtEq
            | BinaryOp::In
            | BinaryOp::StartsWith
            | BinaryOp::EndsWith
            | BinaryOp::Contains => compare_op(&a, op, &b)?,
            BinaryOp::RegexMatch => match (&a, &b) {
                (Value::String(x), Value::String(pattern)) => {
                    if !self.regexes.contains_key(pattern) {
                        let re = regex::Regex::new(&format!("^(?:{pattern})$"))
                            .map_err(|e| invalid(format!("invalid regular expression: {e}")))?;
                        self.regexes.insert(pattern.clone(), re);
                    }
                    Value::Bool(self.regexes[pattern].is_match(x))
                }
                _ => Value::Null,
            },
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Mod
            | BinaryOp::Pow => arithmetic(a, op, b)?,
            BinaryOp::And | BinaryOp::Or => unreachable!("handled above"),
        })
    }
}
