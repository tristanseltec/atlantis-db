//! The scalar (non-aggregate) function library.
//!
//! [`Exec::call_function`] lower-cases the name once and dispatches to one
//! family module. A few functions handle `null` themselves; every other
//! function returns `null` for a `null` first argument.

mod graph;
mod list;
mod math;
mod spatial;
mod string;
mod temporal;

use std::borrow::Cow;

use atlantis_core::{AtlantisError, Value};

use super::invalid;
use crate::exec::Exec;

/// ASCII-lowercases a function or procedure name, borrowing it when it is
/// already lower case.
pub(crate) fn lowercase(name: &str) -> Cow<'_, str> {
    if name.bytes().any(|b| b.is_ascii_uppercase()) {
        Cow::Owned(name.to_ascii_lowercase())
    } else {
        Cow::Borrowed(name)
    }
}

/// Fails unless `name()` was called with exactly `n` arguments.
fn arity(name: &str, args: &[Value], n: usize) -> Result<(), AtlantisError> {
    if args.len() == n {
        Ok(())
    } else {
        Err(invalid(format!(
            "{name}() takes {n} argument(s), got {}",
            args.len()
        )))
    }
}

/// The first argument, moved out (`null` when there is none).
fn first(args: Vec<Value>) -> Value {
    args.into_iter().next().unwrap_or(Value::Null)
}

/// The error for a name no family knows.
fn unknown(name: &str) -> AtlantisError {
    invalid(format!("unknown function {name}()"))
}

impl Exec<'_> {
    /// Calls a scalar (non-aggregate) function.
    pub(crate) fn call_function(
        &mut self,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Value, AtlantisError> {
        let lname = lowercase(name);
        let lname = lname.as_ref();
        // Most functions return null for a null first argument.
        let null_in = args.first().is_none_or(Value::is_null);
        match lname {
            "coalesce" => Ok(args
                .into_iter()
                .find(|v| !v.is_null())
                .unwrap_or(Value::Null)),
            "exists" => {
                arity(name, &args, 1)?;
                Ok(Value::Bool(!first(args).is_null()))
            }
            "timestamp" => Ok(temporal::timestamp()),
            "time.now" | "datetime" | "datetime.realtime" if args.is_empty() => Ok(temporal::now()),
            "rid" => graph::rid(name, args),
            "point" => spatial::point(name, args),
            _ if null_in && lname != "range" => Ok(Value::Null),
            "id" | "elementid" | "labels" | "type" | "startnode" | "endnode" | "properties"
            | "keys" | "nodes" | "relationships" | "length" => {
                self.graph_function(name, lname, args)
            }
            "size" | "head" | "last" | "tail" | "reverse" | "range" => {
                list::call(name, lname, args)
            }
            "toupper" | "upper" | "tolower" | "lower" | "trim" | "ltrim" | "rtrim" | "replace"
            | "split" | "substring" | "left" | "right" | "tostring" | "tointeger" | "tofloat"
            | "toboolean" => string::call(name, lname, args),
            "abs"
            | "ceil"
            | "floor"
            | "round"
            | "sqrt"
            | "sign"
            | "exp"
            | "log"
            | "log10"
            | "db.vector.similarity"
            | "vector.similarity.cosine" => math::call(name, lname, args),
            "datetime" | "duration" => temporal::call(name, lname, args),
            "point.distance" | "distance" => spatial::distance(name, args),
            _ => Err(unknown(name)),
        }
    }
}
