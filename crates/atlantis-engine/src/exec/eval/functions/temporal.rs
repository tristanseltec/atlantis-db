//! Temporal functions: the current time, `datetime()` and `duration()`.

use atlantis_core::{AtlantisError, Datetime, Duration, Value};

use super::{first, invalid, unknown};

/// `timestamp()`: milliseconds since the Unix epoch.
pub(super) fn timestamp() -> Value {
    Value::Integer(Datetime::now().0.div_euclid(1_000_000))
}

/// `datetime()` / `time.now()` without arguments.
pub(super) fn now() -> Value {
    Value::Datetime(Datetime::now())
}

/// `datetime(text)` and `duration(text)`; `lname` is the lower-cased `name`.
pub(super) fn call(name: &str, lname: &str, args: Vec<Value>) -> Result<Value, AtlantisError> {
    Ok(match lname {
        "datetime" => match first(args) {
            Value::String(s) => Value::Datetime(s.parse()?),
            Value::Datetime(d) => Value::Datetime(d),
            other => {
                return Err(invalid(format!(
                    "datetime() expects a string, got {}",
                    other.type_name()
                )))
            }
        },
        "duration" => match first(args) {
            Value::String(s) => Value::Duration(s.parse::<Duration>()?),
            Value::Duration(d) => Value::Duration(d),
            other => {
                return Err(invalid(format!(
                    "duration() expects a string, got {}",
                    other.type_name()
                )))
            }
        },
        _ => return Err(unknown(name)),
    })
}
