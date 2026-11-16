//! List functions: `size`, `head`, `last`, `tail`, `reverse`, `range`.

use atlantis_core::{AtlantisError, Value};

use super::{first, invalid, unknown};
use crate::exec::eval::int_arg;

/// List functions; `lname` is the lower-cased `name`.
pub(super) fn call(name: &str, lname: &str, args: Vec<Value>) -> Result<Value, AtlantisError> {
    Ok(match lname {
        "size" => match first(args) {
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
        "head" | "last" => match first(args) {
            Value::Array(mut items) => {
                let item = if lname == "head" {
                    items.into_iter().next()
                } else {
                    items.pop()
                };
                item.unwrap_or(Value::Null)
            }
            other => {
                return Err(invalid(format!(
                    "{name}() expects a list, got {}",
                    other.type_name()
                )))
            }
        },
        "tail" => match first(args) {
            Value::Array(items) => Value::Array(items.into_iter().skip(1).collect()),
            other => {
                return Err(invalid(format!(
                    "tail() expects a list, got {}",
                    other.type_name()
                )))
            }
        },
        "reverse" => match first(args) {
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
        "range" => range(&args)?,
        _ => return Err(unknown(name)),
    })
}

/// `range(start, end[, step])`, both bounds inclusive.
fn range(args: &[Value]) -> Result<Value, AtlantisError> {
    if !(2..=3).contains(&args.len()) {
        return Err(invalid("range() takes 2 or 3 arguments"));
    }
    let start = int_arg(&args[0], "range start")?
        .ok_or_else(|| invalid("range() bounds cannot be null"))?;
    let end =
        int_arg(&args[1], "range end")?.ok_or_else(|| invalid("range() bounds cannot be null"))?;
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
    Ok(Value::Array(out))
}
