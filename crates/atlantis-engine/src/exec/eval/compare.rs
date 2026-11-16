//! Comparison semantics: Cypher equality, ordering, the comparison
//! operators and the canonical keys used for grouping and `DISTINCT`.

use std::{cmp::Ordering, fmt::Write};

use atlantis_core::{AtlantisError, Value};
use atlantis_cypher::ast::BinaryOp;

use super::invalid;

/// Rank used to order values of different types (`ORDER BY`, `min`, `max`).
fn type_rank(v: &Value) -> u8 {
    match v {
        Value::Object(_) => 0,
        Value::RecordId(_) => 1,
        Value::Array(_) | Value::Vector(_) => 2,
        Value::Geometry(_) => 3,
        Value::String(_) => 4,
        Value::Bool(_) => 5,
        Value::Integer(_) | Value::Float(_) => 6,
        Value::Datetime(_) => 7,
        Value::Duration(_) => 8,
        Value::Uuid(_) => 9,
        Value::Bytes(_) => 10,
        Value::Null => 11,
    }
}

/// Total order for sorting: by type rank, then by value; `null` sorts last.
pub(crate) fn order_cmp(a: &Value, b: &Value) -> Ordering {
    let (ra, rb) = (type_rank(a), type_rank(b));
    if ra != rb {
        return ra.cmp(&rb);
    }
    match (a, b) {
        (Value::Float(x), Value::Float(y)) => x.total_cmp(y),
        (Value::Array(x), Value::Array(y)) => {
            for (p, q) in x.iter().zip(y) {
                let c = order_cmp(p, q);
                if c != Ordering::Equal {
                    return c;
                }
            }
            x.len().cmp(&y.len())
        }
        _ => a.partial_cmp(b).unwrap_or(Ordering::Equal),
    }
}

/// A canonical key for grouping and `DISTINCT` (equal values, equal keys).
pub(crate) fn group_key(values: &[Value]) -> String {
    /// Appends the canonical form of `v` to `out`.
    fn norm(out: &mut String, v: &Value) {
        match v {
            // 1 and 1.0 are equal in Cypher, so they must group together.
            Value::Float(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => {
                let _ = write!(out, "{:?}", Value::Integer(*f as i64));
            }
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    norm(out, item);
                }
                out.push(']');
            }
            Value::Object(obj) => {
                out.push('{');
                for (i, (k, v)) in obj.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    let _ = write!(out, "{k:?}:");
                    norm(out, v);
                }
                out.push('}');
            }
            other => {
                let _ = write!(out, "{other:?}");
            }
        }
    }
    let mut out = String::new();
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            out.push('\u{1f}');
        }
        norm(&mut out, v);
    }
    out
}

/// Cypher equality: `null` if either side is (or, inside lists and maps,
/// if the result depends on) a `null`; otherwise `true`/`false`.
pub(super) fn cypher_eq(a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Value::Bool(false);
            }
            let mut unknown = false;
            for (p, q) in x.iter().zip(y) {
                match cypher_eq(p, q) {
                    Value::Bool(false) => return Value::Bool(false),
                    Value::Null => unknown = true,
                    _ => {}
                }
            }
            if unknown {
                Value::Null
            } else {
                Value::Bool(true)
            }
        }
        (Value::Object(x), Value::Object(y)) => {
            if x.len() != y.len() || x.keys().ne(y.keys()) {
                return Value::Bool(false);
            }
            let mut unknown = false;
            for (p, q) in x.values().zip(y.values()) {
                match cypher_eq(p, q) {
                    Value::Bool(false) => return Value::Bool(false),
                    Value::Null => unknown = true,
                    _ => {}
                }
            }
            if unknown {
                Value::Null
            } else {
                Value::Bool(true)
            }
        }
        _ => Value::Bool(a == b),
    }
}

fn compare(a: &Value, b: &Value, op: BinaryOp) -> Value {
    if a.is_null() || b.is_null() {
        return Value::Null;
    }
    let ord = match (a, b) {
        (Value::String(_), Value::String(_))
        | (Value::Integer(_) | Value::Float(_), Value::Integer(_) | Value::Float(_))
        | (Value::Bool(_), Value::Bool(_))
        | (Value::Datetime(_), Value::Datetime(_))
        | (Value::Duration(_), Value::Duration(_))
        | (Value::Array(_), Value::Array(_)) => a.partial_cmp(b),
        _ => None,
    };
    let Some(ord) = ord else { return Value::Null };
    Value::Bool(match op {
        BinaryOp::Lt => ord == Ordering::Less,
        BinaryOp::LtEq => ord != Ordering::Greater,
        BinaryOp::Gt => ord == Ordering::Greater,
        BinaryOp::GtEq => ord != Ordering::Less,
        _ => unreachable!("not a comparison"),
    })
}

/// `true` for the operators [`compare_op`] evaluates.
pub(crate) fn is_comparison(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq
            | BinaryOp::NotEq
            | BinaryOp::Lt
            | BinaryOp::LtEq
            | BinaryOp::Gt
            | BinaryOp::GtEq
            | BinaryOp::In
            | BinaryOp::StartsWith
            | BinaryOp::EndsWith
            | BinaryOp::Contains
    )
}

/// Evaluates `a op b` for a comparison operator ([`is_comparison`]), with
/// Cypher's three-valued logic. `WHERE` and the vector-search filter
/// pushdown both use it, so a pushed-down filter admits exactly the rows
/// the `WHERE` keeps.
pub(crate) fn compare_op(a: &Value, op: BinaryOp, b: &Value) -> Result<Value, AtlantisError> {
    Ok(match op {
        BinaryOp::Eq => cypher_eq(a, b),
        BinaryOp::NotEq => match cypher_eq(a, b) {
            Value::Bool(x) => Value::Bool(!x),
            other => other,
        },
        BinaryOp::Lt | BinaryOp::LtEq | BinaryOp::Gt | BinaryOp::GtEq => compare(a, b, op),
        BinaryOp::In => match b {
            Value::Null => Value::Null,
            Value::Array(items) => {
                if a.is_null() {
                    return Ok(if items.is_empty() {
                        Value::Bool(false)
                    } else {
                        Value::Null
                    });
                }
                let mut saw_null = false;
                for item in items {
                    match cypher_eq(a, item) {
                        Value::Bool(true) => return Ok(Value::Bool(true)),
                        Value::Null => saw_null = true,
                        _ => {}
                    }
                }
                if saw_null {
                    Value::Null
                } else {
                    Value::Bool(false)
                }
            }
            other => {
                return Err(invalid(format!(
                    "IN expects a list, got {}",
                    other.type_name()
                )))
            }
        },
        BinaryOp::StartsWith | BinaryOp::EndsWith | BinaryOp::Contains => match (a, b) {
            (Value::String(x), Value::String(y)) => Value::Bool(match op {
                BinaryOp::StartsWith => x.starts_with(y.as_str()),
                BinaryOp::EndsWith => x.ends_with(y.as_str()),
                _ => x.contains(y.as_str()),
            }),
            _ => Value::Null,
        },
        other => return Err(invalid(format!("{other:?} is not a comparison"))),
    })
}
