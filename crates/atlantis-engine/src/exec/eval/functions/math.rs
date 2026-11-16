//! Numeric functions and vector similarity.

use atlantis_core::{AtlantisError, Value};

use super::{arity, first, invalid, unknown};
use crate::exec::eval::as_vector;

/// Math functions; `lname` is the lower-cased `name`.
pub(super) fn call(name: &str, lname: &str, args: Vec<Value>) -> Result<Value, AtlantisError> {
    Ok(match lname {
        "abs" => match first(args) {
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
            let x = args
                .first()
                .and_then(Value::as_float)
                .ok_or_else(|| invalid(format!("{name}() expects a number")))?;
            Value::Float(match lname {
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
        "db.vector.similarity" | "vector.similarity.cosine" => {
            arity(name, &args, 2)?;
            cosine(name, &args[0], &args[1])?
        }
        _ => return Err(unknown(name)),
    })
}

/// Cosine similarity of two equally long vectors (`null` for a zero vector).
fn cosine(name: &str, a: &Value, b: &Value) -> Result<Value, AtlantisError> {
    match (as_vector(a), as_vector(b)) {
        (Some(a), Some(b)) if a.len() == b.len() => {
            let dot: f64 = a
                .iter()
                .zip(&b)
                .map(|(x, y)| (*x as f64) * (*y as f64))
                .sum();
            let na: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
            let nb: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
            Ok(if na == 0.0 || nb == 0.0 {
                Value::Null
            } else {
                Value::Float(dot / (na * nb))
            })
        }
        (Some(a), Some(b)) => Err(AtlantisError::DimensionMismatch {
            expected: a.len(),
            actual: b.len(),
        }),
        _ => Err(invalid(format!("{name}() expects two vectors"))),
    }
}
