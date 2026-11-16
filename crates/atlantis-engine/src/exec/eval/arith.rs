//! Arithmetic on numbers, strings, lists and temporal values.

use atlantis_core::{AtlantisError, Value};
use atlantis_cypher::ast::BinaryOp;

use super::invalid;

pub(super) fn arithmetic(a: Value, op: BinaryOp, b: Value) -> Result<Value, AtlantisError> {
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
