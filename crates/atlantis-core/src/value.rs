//! The universal property value type for Atlantis DB.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

/// A property value that can be stored on nodes and relationships.
///
/// Variants cover the standard property types plus a dedicated `Vector`
/// variant for embedding data used by the HNSW index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Value {
    /// Absence of a value.
    Null,
    /// Boolean true/false.
    Bool(bool),
    /// 64-bit signed integer.
    Integer(i64),
    /// 64-bit floating point.
    Float(f64),
    /// UTF-8 string.
    String(String),
    /// Array of values (heterogeneous).
    Array(Vec<Value>),
    /// A fixed-dimension float vector for embedding similarity search.
    /// Distinct from `Array` to enable type-checking and efficient HNSW storage.
    Vector(Vec<f32>),
}

impl Value {
    /// Returns the value as a `&str` if it is a `String`.
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Returns the value as an `i64` if it is an `Integer`.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Integer(i) => Some(*i),
            _ => None,
        }
    }

    /// Returns the value as an `f64` if it is `Float` or `Integer` (widened).
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// Returns the value as a `&[f32]` slice if it is a `Vector`.
    pub fn as_vector(&self) -> Option<&[f32]> {
        match self {
            Value::Vector(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    /// Returns the value as a `bool` if it is `Bool`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Returns `true` if the value is `Null`.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Returns `true` if the value is a `Vector`.
    pub fn is_vector(&self) -> bool {
        matches!(self, Value::Vector(_))
    }

    /// Returns the type name as a string, for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Integer(_) => "integer",
            Value::Float(_) => "float",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Vector(_) => "vector",
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Integer(a), Value::Integer(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => a == b,
            (Value::Vector(a), Value::Vector(b)) => a == b,
            // Cross-type numeric comparison: Integer vs Float
            (Value::Integer(a), Value::Float(b)) => (*a as f64) == *b,
            (Value::Float(a), Value::Integer(b)) => *a == (*b as f64),
            _ => false,
        }
    }
}

impl Eq for Value {}

impl PartialOrd for Value {
    /// Provides ordering for WHERE clause comparisons.
    ///
    /// `Null` is not comparable and returns `None` when compared with any
    /// non-null value. Numeric types (Integer/Float) are cross-comparable.
    /// Different types are not comparable (returns `None`).
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Value::Null, Value::Null) => Some(Ordering::Equal),
            (Value::Null, _) | (_, Value::Null) => None,
            (Value::Bool(a), Value::Bool(b)) => a.partial_cmp(b),
            (Value::Integer(a), Value::Integer(b)) => Some(a.cmp(b)),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::Integer(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
            (Value::Float(a), Value::Integer(b)) => a.partial_cmp(&(*b as f64)),
            (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
            (Value::Array(a), Value::Array(b)) => a.partial_cmp(b),
            (Value::Vector(a), Value::Vector(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Integer(i) => write!(f, "{i}"),
            Value::Float(fl) => write!(f, "{fl}"),
            Value::String(s) => write!(f, "'{s}'"),
            Value::Array(arr) => {
                write!(f, "[")?;
                for (i, v) in arr.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Value::Vector(v) => {
                write!(f, "[")?;
                for (i, x) in v.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{x}")?;
                }
                write!(f, "]")
            }
        }
    }
}

// ---- Convenience From impls ----

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Integer(v)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Integer(v as i64)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}

impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::Float(v as f64)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::String(v.to_string())
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::String(v)
    }
}

impl From<Vec<f32>> for Value {
    fn from(v: Vec<f32>) -> Self {
        Value::Vector(v)
    }
}

impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Self {
        Value::Array(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_string() {
        let v = Value::String("hello".into());
        assert_eq!(v.as_string(), Some("hello"));
        assert_eq!(Value::Integer(42).as_string(), None);
    }

    #[test]
    fn test_as_int() {
        assert_eq!(Value::Integer(42).as_int(), Some(42));
        assert_eq!(Value::Float(3.14).as_int(), None);
    }

    #[test]
    fn test_as_float_cross_type() {
        assert_eq!(Value::Float(3.14).as_float(), Some(3.14));
        assert_eq!(Value::Integer(42).as_float(), Some(42.0));
        assert_eq!(Value::String("x".into()).as_float(), None);
    }

    #[test]
    fn test_as_vector() {
        let v = Value::Vector(vec![0.1, 0.2, 0.3]);
        assert_eq!(v.as_vector(), Some([0.1, 0.2, 0.3].as_slice()));
        assert_eq!(Value::Integer(1).as_vector(), None);
    }

    #[test]
    fn test_as_bool() {
        assert_eq!(Value::Bool(true).as_bool(), Some(true));
        assert_eq!(Value::Bool(false).as_bool(), Some(false));
        assert_eq!(Value::Null.as_bool(), None);
    }

    #[test]
    fn test_is_null() {
        assert!(Value::Null.is_null());
        assert!(!Value::Integer(0).is_null());
    }

    #[test]
    fn test_is_vector() {
        assert!(Value::Vector(vec![1.0]).is_vector());
        assert!(!Value::Array(vec![Value::Integer(1)]).is_vector());
    }

    #[test]
    fn test_partial_eq_same_type() {
        assert_eq!(Value::Integer(42), Value::Integer(42));
        assert_ne!(Value::Integer(42), Value::Integer(43));
        assert_eq!(Value::String("hi".into()), Value::String("hi".into()));
        assert_eq!(Value::Null, Value::Null);
    }

    #[test]
    fn test_partial_eq_cross_numeric() {
        assert_eq!(Value::Integer(42), Value::Float(42.0));
        assert_eq!(Value::Float(42.0), Value::Integer(42));
        assert_ne!(Value::Integer(42), Value::Float(42.5));
    }

    #[test]
    fn test_partial_eq_different_types() {
        assert_ne!(Value::Integer(42), Value::String("42".into()));
        assert_ne!(Value::Bool(true), Value::Integer(1));
    }

    #[test]
    fn test_partial_ord() {
        assert!(Value::Integer(1) < Value::Integer(2));
        assert!(Value::Float(1.5) > Value::Float(1.0));
        assert!(Value::String("a".into()) < Value::String("b".into()));
    }

    #[test]
    fn test_partial_ord_cross_numeric() {
        assert!(Value::Integer(1) < Value::Float(1.5));
        assert!(Value::Float(1.5) > Value::Integer(1));
    }

    #[test]
    fn test_null_not_comparable() {
        assert_eq!(Value::Null.partial_cmp(&Value::Integer(1)), None);
        assert_eq!(Value::Integer(1).partial_cmp(&Value::Null), None);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", Value::Null), "null");
        assert_eq!(format!("{}", Value::Bool(true)), "true");
        assert_eq!(format!("{}", Value::Integer(42)), "42");
        assert_eq!(format!("{}", Value::String("hi".into())), "'hi'");
    }

    #[test]
    fn test_from_impls() {
        assert_eq!(Value::from(true), Value::Bool(true));
        assert_eq!(Value::from(42i64), Value::Integer(42));
        assert_eq!(Value::from(42i32), Value::Integer(42));
        assert_eq!(Value::from(3.14f64), Value::Float(3.14));
        assert_eq!(Value::from("hi"), Value::String("hi".into()));
        assert_eq!(Value::from(vec![1.0f32]), Value::Vector(vec![1.0]));
    }

    #[test]
    fn test_type_name() {
        assert_eq!(Value::Null.type_name(), "null");
        assert_eq!(Value::Bool(true).type_name(), "boolean");
        assert_eq!(Value::Integer(1).type_name(), "integer");
        assert_eq!(Value::Float(1.0).type_name(), "float");
        assert_eq!(Value::String("x".into()).type_name(), "string");
        assert_eq!(Value::Array(vec![]).type_name(), "array");
        assert_eq!(Value::Vector(vec![]).type_name(), "vector");
    }

    #[test]
    fn test_serde_roundtrip() {
        let values = vec![
            Value::Null,
            Value::Bool(true),
            Value::Integer(42),
            Value::Float(3.14),
            Value::String("hello".into()),
            Value::Array(vec![Value::Integer(1), Value::String("two".into())]),
            Value::Vector(vec![0.1, 0.2, 0.3]),
        ];

        for v in &values {
            let serialized = bincode::serialize(v).unwrap();
            let deserialized: Value = bincode::deserialize(&serialized).unwrap();
            assert_eq!(v, &deserialized);
        }
    }
}
