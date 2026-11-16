//! Aggregation: `count`, `collect`, `min`, `max`, `sum`, `avg` and `stdev`
//! over a group of rows, substituted into the projected expression.

use std::collections::HashSet;

use atlantis_core::{AtlantisError, Value};
use atlantis_cypher::ast::Expression;

use super::{
    eval::{group_key, invalid, lowercase, order_cmp},
    Exec, Row,
};

impl Exec<'_> {
    /// Replaces every aggregate call in `expr` with its value over `group`.
    pub(super) fn substitute_aggregates(
        &mut self,
        expr: &Expression,
        group: &[Row],
    ) -> Result<Expression, AtlantisError> {
        use Expression as E;
        Ok(match expr {
            E::CountStar => E::Literal(Value::Integer(group.len() as i64)),
            E::Function {
                name,
                distinct,
                args,
            } if atlantis_cypher::ast::is_aggregate(name) => {
                E::Literal(self.aggregate(name, *distinct, args, group)?)
            }
            E::Function {
                name,
                distinct,
                args,
            } => E::Function {
                name: name.clone(),
                distinct: *distinct,
                args: args
                    .iter()
                    .map(|a| self.substitute_aggregates(a, group))
                    .collect::<Result<_, _>>()?,
            },
            E::Property(b, k) => {
                E::Property(Box::new(self.substitute_aggregates(b, group)?), k.clone())
            }
            E::Index(a, b) => E::Index(
                Box::new(self.substitute_aggregates(a, group)?),
                Box::new(self.substitute_aggregates(b, group)?),
            ),
            E::Unary(op, e) => E::Unary(*op, Box::new(self.substitute_aggregates(e, group)?)),
            E::Binary(a, op, b) => E::Binary(
                Box::new(self.substitute_aggregates(a, group)?),
                *op,
                Box::new(self.substitute_aggregates(b, group)?),
            ),
            E::IsNull(e, n) => E::IsNull(Box::new(self.substitute_aggregates(e, group)?), *n),
            E::List(items) => E::List(
                items
                    .iter()
                    .map(|i| self.substitute_aggregates(i, group))
                    .collect::<Result<_, _>>()?,
            ),
            E::Map(items) => E::Map(
                items
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), self.substitute_aggregates(v, group)?)))
                    .collect::<Result<_, AtlantisError>>()?,
            ),
            E::Case {
                operand,
                whens,
                default,
            } => E::Case {
                operand: operand
                    .as_ref()
                    .map(|o| self.substitute_aggregates(o, group).map(Box::new))
                    .transpose()?,
                whens: whens
                    .iter()
                    .map(|(a, b)| {
                        Ok((
                            self.substitute_aggregates(a, group)?,
                            self.substitute_aggregates(b, group)?,
                        ))
                    })
                    .collect::<Result<_, AtlantisError>>()?,
                default: default
                    .as_ref()
                    .map(|d| self.substitute_aggregates(d, group).map(Box::new))
                    .transpose()?,
            },
            other => {
                if other.contains_aggregate() {
                    return Err(invalid("aggregates are not supported in this position"));
                }
                other.clone()
            }
        })
    }

    fn aggregate(
        &mut self,
        name: &str,
        distinct: bool,
        args: &[Expression],
        group: &[Row],
    ) -> Result<Value, AtlantisError> {
        let [arg] = args else {
            return Err(invalid(format!("{name}() takes exactly one argument")));
        };
        if arg.contains_aggregate() {
            return Err(invalid("aggregates cannot be nested"));
        }
        let mut values = Vec::new();
        let mut seen = HashSet::new();
        for row in group {
            let v = self.eval(arg, row)?;
            if v.is_null() {
                continue;
            }
            if distinct && !seen.insert(group_key(std::slice::from_ref(&v))) {
                continue;
            }
            values.push(v);
        }
        let lname = lowercase(name);
        let lname = lname.as_ref();
        Ok(match lname {
            "count" => Value::Integer(values.len() as i64),
            "collect" => Value::Array(values),
            "min" => values.into_iter().min_by(order_cmp).unwrap_or(Value::Null),
            "max" => values.into_iter().max_by(order_cmp).unwrap_or(Value::Null),
            "sum" | "avg" | "stdev" => {
                if values.is_empty() {
                    return Ok(if lname == "sum" {
                        Value::Integer(0)
                    } else {
                        Value::Null
                    });
                }
                if lname == "sum" && values.iter().all(|v| matches!(v, Value::Integer(_))) {
                    let mut total: i64 = 0;
                    for v in &values {
                        total = total
                            .checked_add(v.as_int().expect("integer"))
                            .ok_or_else(|| invalid("integer overflow in sum()"))?;
                    }
                    return Ok(Value::Integer(total));
                }
                let nums: Vec<f64> = values
                    .iter()
                    .map(|v| {
                        v.as_float().ok_or_else(|| {
                            invalid(format!("{name}() expects numbers, got {}", v.type_name()))
                        })
                    })
                    .collect::<Result<_, _>>()?;
                let sum: f64 = nums.iter().sum();
                let mean = sum / nums.len() as f64;
                match lname {
                    "sum" => Value::Float(sum),
                    "avg" => Value::Float(mean),
                    _ if nums.len() < 2 => Value::Float(0.0),
                    _ => Value::Float(
                        (nums.iter().map(|x| (x - mean).powi(2)).sum::<f64>()
                            / (nums.len() - 1) as f64)
                            .sqrt(),
                    ),
                }
            }
            _ => return Err(invalid(format!("unknown aggregate {name}()"))),
        })
    }
}
