//! Expression evaluation.
//!
//! Semantics follow Cypher: `null` propagates through operators and
//! comparisons, boolean operators use three-valued logic, and property
//! access on a record id loads the record (so stored record links
//! dereference transparently).

use std::cmp::Ordering;

use atlantis_core::{
    AtlantisError, Datetime, Duration, Geometry, Object, Point, Record, RecordId, RecordKey, Value,
};
use atlantis_cypher::ast::{BinaryOp, Expression, UnaryOp};

use super::{Exec, Row};

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
    fn norm(v: &Value) -> String {
        match v {
            // 1 and 1.0 are equal in Cypher, so they must group together.
            Value::Float(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => {
                format!("{:?}", Value::Integer(*f as i64))
            }
            Value::Array(items) => {
                format!("[{}]", items.iter().map(norm).collect::<Vec<_>>().join(","))
            }
            Value::Object(obj) => format!(
                "{{{}}}",
                obj.iter()
                    .map(|(k, v)| format!("{k:?}:{}", norm(v)))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            other => format!("{other:?}"),
        }
    }
    values.iter().map(norm).collect::<Vec<_>>().join("\u{1f}")
}

/// Cypher equality: `null` if either side is (or, inside lists and maps,
/// if the result depends on) a `null`; otherwise `true`/`false`.
pub(crate) fn cypher_eq(a: &Value, b: &Value) -> Value {
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

fn arithmetic(a: Value, op: BinaryOp, b: Value) -> Result<Value, AtlantisError> {
    use Value::*;
    if a.is_null() || b.is_null() {
        return Ok(Null);
    }
    let overflow = || invalid("integer overflow");
    Ok(match (op, a, b) {
        (BinaryOp::Add, String(x), y) => String(format!("{x}{}", plain(&y))),
        (BinaryOp::Add, x, String(y)) => String(format!("{}{y}", plain(&x))),
        (BinaryOp::Add, Array(mut x), Array(y)) => {
            x.extend(y);
            Array(x)
        }
        (BinaryOp::Add, Array(mut x), y) => {
            x.push(y);
            Array(x)
        }
        (BinaryOp::Add, x, Array(mut y)) => {
            y.insert(0, x);
            Array(y)
        }
        (BinaryOp::Add, Datetime(d), Duration(e)) | (BinaryOp::Add, Duration(e), Datetime(d)) => {
            Datetime(d.checked_add(e).ok_or_else(overflow)?)
        }
        (BinaryOp::Sub, Datetime(d), Duration(e)) => Datetime(atlantis_core::Datetime(
            d.0.checked_sub(e.0).ok_or_else(overflow)?,
        )),
        (BinaryOp::Sub, Datetime(a), Datetime(b)) => Duration(atlantis_core::Duration(
            a.0.checked_sub(b.0).ok_or_else(overflow)?,
        )),
        (BinaryOp::Add, Duration(a), Duration(b)) => Duration(atlantis_core::Duration(
            a.0.checked_add(b.0).ok_or_else(overflow)?,
        )),
        (BinaryOp::Sub, Duration(a), Duration(b)) => Duration(atlantis_core::Duration(
            a.0.checked_sub(b.0).ok_or_else(overflow)?,
        )),
        (op, Integer(x), Integer(y)) => match op {
            BinaryOp::Add => Integer(x.checked_add(y).ok_or_else(overflow)?),
            BinaryOp::Sub => Integer(x.checked_sub(y).ok_or_else(overflow)?),
            BinaryOp::Mul => Integer(x.checked_mul(y).ok_or_else(overflow)?),
            BinaryOp::Div if y == 0 => return Err(invalid("division by zero")),
            BinaryOp::Div => Integer(x.checked_div(y).ok_or_else(overflow)?),
            BinaryOp::Mod if y == 0 => return Err(invalid("division by zero")),
            BinaryOp::Mod => Integer(x.checked_rem(y).ok_or_else(overflow)?),
            BinaryOp::Pow => Float((x as f64).powf(y as f64)),
            _ => unreachable!("not arithmetic"),
        },
        (op, x @ (Integer(_) | Float(_)), y @ (Integer(_) | Float(_))) => {
            let (x, y) = (x.as_float().expect("number"), y.as_float().expect("number"));
            Float(match op {
                BinaryOp::Add => x + y,
                BinaryOp::Sub => x - y,
                BinaryOp::Mul => x * y,
                BinaryOp::Div => x / y,
                BinaryOp::Mod => x % y,
                BinaryOp::Pow => x.powf(y),
                _ => unreachable!("not arithmetic"),
            })
        }
        (op, x, y) => {
            return Err(invalid(format!(
                "cannot apply {op:?} to {} and {}",
                x.type_name(),
                y.type_name()
            )))
        }
    })
}

/// String form used by concatenation and `toString` (no quotes).
pub(crate) fn plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Datetime(d) => d.to_string(),
        Value::Uuid(u) => u.to_string(),
        other => other.to_string(),
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

fn list_index(len: usize, i: i64) -> Option<usize> {
    let idx = if i < 0 { len as i64 + i } else { i };
    (0..len as i64).contains(&idx).then_some(idx as usize)
}

fn haversine_m(a: &Point, b: &Point) -> f64 {
    let r = 6_371_008.8;
    let (la1, la2) = (a.lat.to_radians(), b.lat.to_radians());
    let dlat = la2 - la1;
    let dlon = (b.lon - a.lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * r * h.sqrt().asin()
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
    pub(crate) fn load(&mut self, id: &RecordId) -> Result<Option<Record>, AtlantisError> {
        self.tx.get(id)
    }

    /// Property access on any value.
    pub(crate) fn property(&mut self, base: &Value, key: &str) -> Result<Value, AtlantisError> {
        Ok(match base {
            Value::Null => Value::Null,
            Value::Object(obj) => obj.get(key).cloned().unwrap_or(Value::Null),
            Value::RecordId(id) => {
                if key == "id" {
                    return Ok(Value::RecordId(id.clone()));
                }
                match self.load(id)? {
                    Some(record) => record.data.get(key).cloned().unwrap_or(Value::Null),
                    None => Value::Null,
                }
            }
            Value::Geometry(Geometry::Point(p)) => match key {
                "longitude" | "x" => Value::Float(p.lon),
                "latitude" | "y" => Value::Float(p.lat),
                _ => Value::Null,
            },
            Value::Datetime(d) => {
                let text = d.to_string();
                let num =
                    |r: std::ops::Range<usize>| text.get(r).and_then(|s| s.parse::<i64>().ok());
                match key {
                    "year" => num(0..4).map(Value::Integer).unwrap_or(Value::Null),
                    "month" => num(5..7).map(Value::Integer).unwrap_or(Value::Null),
                    "day" => num(8..10).map(Value::Integer).unwrap_or(Value::Null),
                    "hour" => num(11..13).map(Value::Integer).unwrap_or(Value::Null),
                    "minute" => num(14..16).map(Value::Integer).unwrap_or(Value::Null),
                    "second" => num(17..19).map(Value::Integer).unwrap_or(Value::Null),
                    "epochMillis" => Value::Integer(d.0.div_euclid(1_000_000)),
                    _ => Value::Null,
                }
            }
            other => {
                return Err(invalid(format!(
                    "cannot read property {key} of a {}",
                    other.type_name()
                )))
            }
        })
    }

    /// The labels of a node: its table first, then secondary labels.
    pub(crate) fn labels_of(&mut self, id: &RecordId) -> Result<Vec<String>, AtlantisError> {
        Ok(match self.load(id)? {
            Some(record) => std::iter::once(record.id.table.clone())
                .chain(record.labels)
                .collect(),
            None => Vec::new(),
        })
    }

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
                self.property(&base, key)?
            }
            Expression::Index(base, index) => {
                let (base, index) = (self.eval(base, row)?, self.eval(index, row)?);
                match (&base, &index) {
                    (Value::Null, _) | (_, Value::Null) => Value::Null,
                    (Value::Array(items), _) => {
                        let i = int_arg(&index, "list index")?.unwrap_or(0);
                        list_index(items.len(), i)
                            .map(|i| items[i].clone())
                            .unwrap_or(Value::Null)
                    }
                    (Value::String(s), _) => {
                        let chars: Vec<char> = s.chars().collect();
                        let i = int_arg(&index, "string index")?.unwrap_or(0);
                        list_index(chars.len(), i)
                            .map(|i| Value::String(chars[i].to_string()))
                            .unwrap_or(Value::Null)
                    }
                    (_, Value::String(key)) => self.property(&base, key)?,
                    _ => return Err(invalid(format!("cannot index a {}", base.type_name()))),
                }
            }
            Expression::Slice(base, from, to) => {
                let base = self.eval(base, row)?;
                let bound = |me: &mut Self,
                             e: &Option<Box<Expression>>|
                 -> Result<Option<Option<i64>>, AtlantisError> {
                    match e {
                        None => Ok(Some(None)),
                        Some(e) => {
                            let v = me.eval(e, row)?;
                            Ok(int_arg(&v, "slice bound")?.map(Some))
                        }
                    }
                };
                let (Some(from), Some(to)) = (bound(self, from)?, bound(self, to)?) else {
                    return Ok(Value::Null);
                };
                let Value::Array(items) = base else {
                    return Ok(if base.is_null() {
                        Value::Null
                    } else {
                        return Err(invalid("only lists can be sliced"));
                    });
                };
                let len = items.len() as i64;
                let clamp = |i: i64| (if i < 0 { len + i } else { i }).clamp(0, len) as usize;
                let (a, b) = (clamp(from.unwrap_or(0)), clamp(to.unwrap_or(len)));
                Value::Array(if a < b {
                    items[a..b].to_vec()
                } else {
                    Vec::new()
                })
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
                    scope.insert(variable.clone(), item.clone());
                    if let Some(p) = predicate {
                        if !is_true(&self.eval(p, &scope)?) {
                            continue;
                        }
                    }
                    out.push(match map {
                        Some(m) => self.eval(m, &scope)?,
                        None => item,
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

    /// Calls a scalar (non-aggregate) function.
    pub(crate) fn call_function(
        &mut self,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Value, AtlantisError> {
        let lname = name.to_ascii_lowercase();
        let arity = |n: usize| -> Result<(), AtlantisError> {
            if args.len() == n {
                Ok(())
            } else {
                Err(invalid(format!(
                    "{name}() takes {n} argument(s), got {}",
                    args.len()
                )))
            }
        };
        let first = || args.first().cloned().unwrap_or(Value::Null);
        // Most functions return null for a null first argument.
        let null_in = |args: &[Value]| args.first().is_none_or(Value::is_null);
        Ok(match lname.as_str() {
            "coalesce" => args
                .into_iter()
                .find(|v| !v.is_null())
                .unwrap_or(Value::Null),
            "exists" => {
                arity(1)?;
                Value::Bool(!first().is_null())
            }
            "timestamp" => Value::Integer(Datetime::now().0.div_euclid(1_000_000)),
            "time.now" | "datetime" | "datetime.realtime" if args.is_empty() => {
                Value::Datetime(Datetime::now())
            }
            "rid" => {
                arity(2)?;
                let Value::String(table) = &args[0] else {
                    return Err(invalid("rid() needs a table name string"));
                };
                Value::RecordId(RecordId::new(
                    table.clone(),
                    RecordKey::from_value(&args[1])?,
                )?)
            }
            "point" => {
                arity(1)?;
                let Value::Object(map) = first() else {
                    return Ok(Value::Null);
                };
                let coord =
                    |a: &str, b: &str| map.get(a).or_else(|| map.get(b)).and_then(Value::as_float);
                match (coord("longitude", "x"), coord("latitude", "y")) {
                    (Some(lon), Some(lat)) => {
                        Value::Geometry(Geometry::Point(Point::new(lon, lat)?))
                    }
                    _ => return Err(invalid("point() needs longitude/latitude (or x/y)")),
                }
            }
            _ if null_in(&args) && lname != "range" => Value::Null,
            "id" | "elementid" => {
                arity(1)?;
                match first() {
                    Value::RecordId(id) if lname == "elementid" => Value::String(id.to_string()),
                    Value::RecordId(id) => Value::RecordId(id),
                    other => {
                        return Err(invalid(format!(
                            "id() expects a node or relationship, got {}",
                            other.type_name()
                        )))
                    }
                }
            }
            "labels" => {
                arity(1)?;
                let Value::RecordId(id) = first() else {
                    return Err(invalid("labels() expects a node"));
                };
                Value::Array(
                    self.labels_of(&id)?
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                )
            }
            "type" => {
                arity(1)?;
                let Value::RecordId(id) = first() else {
                    return Err(invalid("type() expects a relationship"));
                };
                Value::String(id.table)
            }
            "startnode" | "endnode" => {
                arity(1)?;
                let Value::RecordId(id) = first() else {
                    return Err(invalid(format!("{name}() expects a relationship")));
                };
                match self.tx.get_edge(&id)? {
                    Some(edge) => Value::RecordId(if lname == "startnode" {
                        edge.from
                    } else {
                        edge.to
                    }),
                    None => Value::Null,
                }
            }
            "properties" => match first() {
                Value::Object(o) => Value::Object(o),
                Value::RecordId(id) => match self.load(&id)? {
                    Some(r) => Value::Object(r.data),
                    None => Value::Null,
                },
                other => {
                    return Err(invalid(format!(
                        "properties() expects a map or record, got {}",
                        other.type_name()
                    )))
                }
            },
            "keys" => {
                let obj = match first() {
                    Value::Object(o) => o,
                    Value::RecordId(id) => self.load(&id)?.map(|r| r.data).unwrap_or_default(),
                    other => {
                        return Err(invalid(format!(
                            "keys() expects a map or record, got {}",
                            other.type_name()
                        )))
                    }
                };
                Value::Array(obj.into_keys().map(Value::String).collect())
            }
            "nodes" | "relationships" => match first() {
                Value::Object(path) => path.get(lname.as_str()).cloned().unwrap_or(Value::Null),
                other => {
                    return Err(invalid(format!(
                        "{name}() expects a path, got {}",
                        other.type_name()
                    )))
                }
            },
            "length" => match first() {
                Value::Object(path) => match path.get("relationships") {
                    Some(Value::Array(rels)) => Value::Integer(rels.len() as i64),
                    _ => return Err(invalid("length() expects a path, list or string")),
                },
                Value::Array(items) => Value::Integer(items.len() as i64),
                Value::String(s) => Value::Integer(s.chars().count() as i64),
                other => {
                    return Err(invalid(format!(
                        "length() expects a path, got {}",
                        other.type_name()
                    )))
                }
            },
            "size" => match first() {
                Value::Array(items) => Value::Integer(items.len() as i64),
                Value::Vector(v) => Value::Integer(v.len() as i64),
                Value::String(s) => Value::Integer(s.chars().count() as i64),
                Value::Object(o) => Value::Integer(o.len() as i64),
                other => {
                    return Err(invalid(format!(
                        "size() expects a list or string, got {}",
                        other.type_name()
                    )))
                }
            },
            "head" | "last" => match first() {
                Value::Array(items) => {
                    let item = if lname == "head" {
                        items.first()
                    } else {
                        items.last()
                    };
                    item.cloned().unwrap_or(Value::Null)
                }
                other => {
                    return Err(invalid(format!(
                        "{name}() expects a list, got {}",
                        other.type_name()
                    )))
                }
            },
            "tail" => match first() {
                Value::Array(items) => Value::Array(items.into_iter().skip(1).collect()),
                other => {
                    return Err(invalid(format!(
                        "tail() expects a list, got {}",
                        other.type_name()
                    )))
                }
            },
            "reverse" => match first() {
                Value::Array(mut items) => {
                    items.reverse();
                    Value::Array(items)
                }
                Value::String(s) => Value::String(s.chars().rev().collect()),
                other => {
                    return Err(invalid(format!(
                        "reverse() expects a list or string, got {}",
                        other.type_name()
                    )))
                }
            },
            "range" => {
                if !(2..=3).contains(&args.len()) {
                    return Err(invalid("range() takes 2 or 3 arguments"));
                }
                let start = int_arg(&args[0], "range start")?
                    .ok_or_else(|| invalid("range() bounds cannot be null"))?;
                let end = int_arg(&args[1], "range end")?
                    .ok_or_else(|| invalid("range() bounds cannot be null"))?;
                let step = match args.get(2) {
                    Some(v) => int_arg(v, "range step")?.unwrap_or(1),
                    None => 1,
                };
                if step == 0 {
                    return Err(invalid("range() step cannot be 0"));
                }
                let mut out = Vec::new();
                let mut i = start;
                while (step > 0 && i <= end) || (step < 0 && i >= end) {
                    out.push(Value::Integer(i));
                    if out.len() > 10_000_000 {
                        return Err(invalid("range() result is too large"));
                    }
                    i = match i.checked_add(step) {
                        Some(next) => next,
                        None => break,
                    };
                }
                Value::Array(out)
            }
            "toupper" | "upper" | "tolower" | "lower" | "trim" | "ltrim" | "rtrim" => {
                let Value::String(s) = first() else {
                    return Err(invalid(format!("{name}() expects a string")));
                };
                Value::String(match lname.as_str() {
                    "toupper" | "upper" => s.to_uppercase(),
                    "tolower" | "lower" => s.to_lowercase(),
                    "trim" => s.trim().to_string(),
                    "ltrim" => s.trim_start().to_string(),
                    _ => s.trim_end().to_string(),
                })
            }
            "replace" => {
                arity(3)?;
                match (&args[0], &args[1], &args[2]) {
                    (Value::String(s), Value::String(from), Value::String(to)) => {
                        Value::String(s.replace(from.as_str(), to))
                    }
                    _ => Value::Null,
                }
            }
            "split" => {
                arity(2)?;
                match (&args[0], &args[1]) {
                    (Value::String(s), Value::String(sep)) => Value::Array(
                        s.split(sep.as_str())
                            .map(|p| Value::String(p.into()))
                            .collect(),
                    ),
                    _ => Value::Null,
                }
            }
            "substring" | "left" | "right" => {
                let Value::String(s) = first() else {
                    return Err(invalid(format!("{name}() expects a string")));
                };
                let chars: Vec<char> = s.chars().collect();
                let n = |i: usize| {
                    int_arg(args.get(i).unwrap_or(&Value::Null), "length")
                        .map(|v| v.unwrap_or(0).max(0) as usize)
                };
                let out: String = match lname.as_str() {
                    "left" => chars.iter().take(n(1)?).collect(),
                    "right" => chars[chars.len().saturating_sub(n(1)?)..].iter().collect(),
                    _ => {
                        let start = n(1)?;
                        let len = if args.len() > 2 { n(2)? } else { usize::MAX };
                        chars.iter().skip(start).take(len).collect()
                    }
                };
                Value::String(out)
            }
            "tostring" => Value::String(plain(&first())),
            "tointeger" => match first() {
                Value::Integer(i) => Value::Integer(i),
                Value::Float(f) if f.is_finite() => Value::Integer(f.trunc() as i64),
                Value::String(s) => s
                    .trim()
                    .parse::<i64>()
                    .map(Value::Integer)
                    .or_else(|_| {
                        s.trim()
                            .parse::<f64>()
                            .map(|f| Value::Integer(f.trunc() as i64))
                    })
                    .unwrap_or(Value::Null),
                Value::Bool(b) => Value::Integer(b as i64),
                _ => Value::Null,
            },
            "tofloat" => match first() {
                Value::Integer(i) => Value::Float(i as f64),
                Value::Float(f) => Value::Float(f),
                Value::String(s) => s
                    .trim()
                    .parse::<f64>()
                    .map(Value::Float)
                    .unwrap_or(Value::Null),
                _ => Value::Null,
            },
            "toboolean" => match first() {
                Value::Bool(b) => Value::Bool(b),
                Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    _ => Value::Null,
                },
                _ => Value::Null,
            },
            "abs" => match first() {
                Value::Integer(i) => {
                    Value::Integer(i.checked_abs().ok_or_else(|| invalid("integer overflow"))?)
                }
                Value::Float(f) => Value::Float(f.abs()),
                other => {
                    return Err(invalid(format!(
                        "abs() expects a number, got {}",
                        other.type_name()
                    )))
                }
            },
            "ceil" | "floor" | "round" | "sqrt" | "sign" | "exp" | "log" | "log10" => {
                let x = first()
                    .as_float()
                    .ok_or_else(|| invalid(format!("{name}() expects a number")))?;
                Value::Float(match lname.as_str() {
                    "ceil" => x.ceil(),
                    "floor" => x.floor(),
                    "round" => x.round(),
                    "sqrt" => x.sqrt(),
                    "exp" => x.exp(),
                    "log" => x.ln(),
                    "log10" => x.log10(),
                    _ => x.signum(),
                })
            }
            "datetime" => match first() {
                Value::String(s) => Value::Datetime(s.parse()?),
                Value::Datetime(d) => Value::Datetime(d),
                other => {
                    return Err(invalid(format!(
                        "datetime() expects a string, got {}",
                        other.type_name()
                    )))
                }
            },
            "duration" => match first() {
                Value::String(s) => Value::Duration(s.parse::<Duration>()?),
                Value::Duration(d) => Value::Duration(d),
                other => {
                    return Err(invalid(format!(
                        "duration() expects a string, got {}",
                        other.type_name()
                    )))
                }
            },
            "point.distance" | "distance" => {
                arity(2)?;
                match (&args[0], &args[1]) {
                    (Value::Geometry(Geometry::Point(a)), Value::Geometry(Geometry::Point(b))) => {
                        Value::Float(haversine_m(a, b))
                    }
                    _ => Value::Null,
                }
            }
            "db.vector.similarity" | "vector.similarity.cosine" => {
                arity(2)?;
                match (as_vector(&args[0]), as_vector(&args[1])) {
                    (Some(a), Some(b)) if a.len() == b.len() => {
                        let dot: f64 = a
                            .iter()
                            .zip(&b)
                            .map(|(x, y)| (*x as f64) * (*y as f64))
                            .sum();
                        let na: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
                        let nb: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
                        if na == 0.0 || nb == 0.0 {
                            Value::Null
                        } else {
                            Value::Float(dot / (na * nb))
                        }
                    }
                    (Some(a), Some(b)) => {
                        return Err(AtlantisError::DimensionMismatch {
                            expected: a.len(),
                            actual: b.len(),
                        })
                    }
                    _ => return Err(invalid(format!("{name}() expects two vectors"))),
                }
            }
            _ => return Err(invalid(format!("unknown function {name}()"))),
        })
    }
}
