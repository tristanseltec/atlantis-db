//! String functions and type conversions (`toString`, `toInteger`,
//! `toFloat`, `toBoolean`).

use atlantis_core::{AtlantisError, Value};

use super::{arity, first, invalid, unknown};
use crate::exec::eval::{int_arg, plain};

/// String and conversion functions; `lname` is the lower-cased `name`.
pub(super) fn call(name: &str, lname: &str, args: Vec<Value>) -> Result<Value, AtlantisError> {
    Ok(match lname {
        "toupper" | "upper" | "tolower" | "lower" | "trim" | "ltrim" | "rtrim" => {
            let Value::String(s) = first(args) else {
                return Err(invalid(format!("{name}() expects a string")));
            };
            Value::String(match lname {
                "toupper" | "upper" => s.to_uppercase(),
                "tolower" | "lower" => s.to_lowercase(),
                "trim" => s.trim().to_string(),
                "ltrim" => s.trim_start().to_string(),
                _ => s.trim_end().to_string(),
            })
        }
        "replace" => {
            arity(name, &args, 3)?;
            match (&args[0], &args[1], &args[2]) {
                (Value::String(s), Value::String(from), Value::String(to)) => {
                    Value::String(s.replace(from.as_str(), to))
                }
                _ => Value::Null,
            }
        }
        "split" => {
            arity(name, &args, 2)?;
            match (&args[0], &args[1]) {
                (Value::String(s), Value::String(sep)) => Value::Array(
                    s.split(sep.as_str())
                        .map(|p| Value::String(p.into()))
                        .collect(),
                ),
                _ => Value::Null,
            }
        }
        "substring" | "left" | "right" => substring(name, lname, &args)?,
        "tostring" => Value::String(plain(&first(args))),
        "tointeger" => match first(args) {
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
        "tofloat" => match first(args) {
            Value::Integer(i) => Value::Float(i as f64),
            Value::Float(f) => Value::Float(f),
            Value::String(s) => s
                .trim()
                .parse::<f64>()
                .map(Value::Float)
                .unwrap_or(Value::Null),
            _ => Value::Null,
        },
        "toboolean" => match first(args) {
            Value::Bool(b) => Value::Bool(b),
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => Value::Null,
            },
            _ => Value::Null,
        },
        _ => return Err(unknown(name)),
    })
}

/// `substring(s, start[, length])`, `left(s, n)` and `right(s, n)`, counted
/// in characters.
fn substring(name: &str, lname: &str, args: &[Value]) -> Result<Value, AtlantisError> {
    let Some(Value::String(s)) = args.first() else {
        return Err(invalid(format!("{name}() expects a string")));
    };
    let chars: Vec<char> = s.chars().collect();
    let n = |i: usize| {
        int_arg(args.get(i).unwrap_or(&Value::Null), "length")
            .map(|v| v.unwrap_or(0).max(0) as usize)
    };
    let out: String = match lname {
        "left" => chars.iter().take(n(1)?).collect(),
        "right" => chars[chars.len().saturating_sub(n(1)?)..].iter().collect(),
        _ => {
            let start = n(1)?;
            let len = if args.len() > 2 { n(2)? } else { usize::MAX };
            chars.iter().skip(start).take(len).collect()
        }
    };
    Ok(Value::String(out))
}
