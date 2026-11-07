//! The universal property value type for Atlantis DB.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::geometry::Geometry;
use crate::ids::Uuid;
use crate::record::{Object, RecordId};
use crate::temporal::{Datetime, Duration};

/// A value stored in a record field, node property or edge property.
///
/// Variants cover scalars, nested documents (`Object`, `Array`), embeddings
/// (`Vector`), temporal and spatial types, and links to other records
/// (`RecordId`). New variants are appended at the end so that values
/// serialized by earlier versions still decode.
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
    /// Raw bytes.
    Bytes(Vec<u8>),
    /// A UTC instant with nanosecond precision.
    Datetime(Datetime),
    /// A signed length of time.
    Duration(Duration),
    /// A UUID.
    Uuid(Uuid),
    /// A nested document (field name → value).
    Object(Object),
    /// A WGS84 geometry.
    Geometry(Geometry),
    /// A link to another record.
    RecordId(RecordId),
}

impl Value {
    /// Returns the value as an object if it is an `Object`.
    pub fn as_object(&self) -> Option<&Object> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Returns the value as a record id if it is a `RecordId`.
    pub fn as_record_id(&self) -> Option<&RecordId> {
        match self {
            Value::RecordId(id) => Some(id),
            _ => None,
        }
    }

    /// Follows a dotted path into nested objects, e.g. `address.city`.
    pub fn get_path(&self, path: &str) -> Option<&Value> {
        path.split('.')
            .try_fold(self, |v, part| v.as_object()?.get(part))
    }

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
            Value::Bytes(_) => "bytes",
            Value::Datetime(_) => "datetime",
            Value::Duration(_) => "duration",
            Value::Uuid(_) => "uuid",
            Value::Object(_) => "object",
            Value::Geometry(_) => "geometry",
            Value::RecordId(_) => "record",
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
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            (Value::Datetime(a), Value::Datetime(b)) => a == b,
            (Value::Duration(a), Value::Duration(b)) => a == b,
            (Value::Uuid(a), Value::Uuid(b)) => a == b,
            (Value::Object(a), Value::Object(b)) => a == b,
            (Value::Geometry(a), Value::Geometry(b)) => a == b,
            (Value::RecordId(a), Value::RecordId(b)) => a == b,
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
            (Value::Bytes(a), Value::Bytes(b)) => Some(a.cmp(b)),
            (Value::Datetime(a), Value::Datetime(b)) => Some(a.cmp(b)),
            (Value::Duration(a), Value::Duration(b)) => Some(a.cmp(b)),
            (Value::Uuid(a), Value::Uuid(b)) => Some(a.cmp(b)),
            (Value::Object(a), Value::Object(b)) => a.partial_cmp(b),
            (Value::RecordId(a), Value::RecordId(b)) => a.partial_cmp(b),
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
            Value::Bytes(b) => {
                write!(f, "b'")?;
                for byte in b {
                    write!(f, "{byte:02x}")?;
                }
                write!(f, "'")
            }
            Value::Datetime(d) => write!(f, "d'{d}'"),
            Value::Duration(d) => write!(f, "{d}"),
            Value::Uuid(u) => write!(f, "u'{u}'"),
            Value::Object(obj) => {
                write!(f, "{{")?;
                for (i, (k, v)) in obj.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Value::Geometry(g) => write!(f, "{g}"),
            Value::RecordId(id) => write!(f, "{id}"),
        }
    }
}

impl From<Datetime> for Value {
    fn from(v: Datetime) -> Self {
        Value::Datetime(v)
    }
}

impl From<Duration> for Value {
    fn from(v: Duration) -> Self {
        Value::Duration(v)
    }
}

impl From<Uuid> for Value {
    fn from(v: Uuid) -> Self {
        Value::Uuid(v)
    }
}

impl From<Object> for Value {
    fn from(v: Object) -> Self {
        Value::Object(v)
    }
}

impl From<Geometry> for Value {
    fn from(v: Geometry) -> Self {
        Value::Geometry(v)
    }
}

impl From<RecordId> for Value {
    fn from(v: RecordId) -> Self {
        Value::RecordId(v)
    }
}

impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Value::Bytes(v)
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

    #[test]
    fn test_multi_model_variants_roundtrip() {
        use crate::{Geometry, Point, RecordId, Uuid};
        let mut address = Object::new();
        address.insert("city".into(), Value::from("Hue"));
        let mut doc = Object::new();
        doc.insert("address".into(), Value::Object(address));
        doc.insert(
            "owner".into(),
            Value::RecordId("Person:alice".parse::<RecordId>().unwrap()),
        );
        let values = vec![
            Value::Bytes(vec![0xde, 0xad]),
            Value::Datetime("2026-11-07T13:46:10Z".parse().unwrap()),
            Value::Duration("1h30m".parse().unwrap()),
            Value::Uuid(Uuid::new_v4()),
            Value::Object(doc.clone()),
            Value::Geometry(Geometry::Point(Point::new(107.59, 16.46).unwrap())),
            Value::RecordId("Order:42".parse().unwrap()),
        ];
        for v in &values {
            let bytes = bincode::serialize(v).unwrap();
            let back: Value = bincode::deserialize(&bytes).unwrap();
            assert_eq!(v, &back);
        }
        let doc = Value::Object(doc);
        assert_eq!(doc.get_path("address.city"), Some(&Value::from("Hue")));
        assert_eq!(doc.get_path("address.zip"), None);
        assert_eq!(
            doc.get_path("owner")
                .and_then(Value::as_record_id)
                .unwrap()
                .table,
            "Person"
        );
    }

    #[test]
    fn test_legacy_encoding_still_decodes() {
        // Variant indices 0..=6 predate the multi-model types; values written
        // by earlier versions must keep decoding to the same variant.
        let legacy = [
            (Value::Null, 0u32),
            (Value::Integer(7), 2),
            (Value::Vector(vec![1.0]), 6),
        ];
        for (v, index) in legacy {
            let bytes = bincode::serialize(&v).unwrap();
            assert_eq!(u32::from_le_bytes(bytes[..4].try_into().unwrap()), index);
        }
    }

    #[test]
    fn test_new_variant_display() {
        assert_eq!(Value::Bytes(vec![0xde, 0xad]).to_string(), "b'dead'");
        let dt: crate::Datetime = "2026-11-07T00:00:00Z".parse().unwrap();
        assert_eq!(Value::from(dt).to_string(), "d'2026-11-07T00:00:00Z'");
        let id: crate::RecordId = "Person:alice".parse().unwrap();
        assert_eq!(Value::from(id).to_string(), "Person:alice");
        assert_eq!(Value::Uuid(crate::Uuid([0; 16])).type_name(), "uuid");
    }
}
