//! The unified record model.
//!
//! Every piece of data lives in a **namespace → database → table → record**
//! hierarchy. A record is identified by a [`RecordId`] (table + key) and
//! holds a nested document ([`Object`]). Graph nodes are records whose table
//! is their first label; relationships are [`Edge`] records in an edge table
//! named after the relationship type.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::ids::{Ulid, Uuid};
use crate::{AtlantisError, Value};

/// A nested document: field name → value, ordered by field name.
pub type Object = BTreeMap<String, Value>;

/// Default namespace used when none is selected.
pub const DEFAULT_NAMESPACE: &str = "atlantis";
/// Default database used when none is selected.
pub const DEFAULT_DATABASE: &str = "default";
/// Table that holds nodes created without a label.
pub const DEFAULT_TABLE: &str = "_default";

/// Validates a namespace, database, table, label or field identifier:
/// non-empty, at most 255 bytes, ASCII letters, digits and `_`, and not
/// starting with a digit.
pub fn validate_ident(kind: &str, name: &str) -> Result<(), AtlantisError> {
    let ok = !name.is_empty()
        && name.len() <= 255
        && !name.as_bytes()[0].is_ascii_digit()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(AtlantisError::InvalidIdentifier(format!(
            "invalid {kind} name: {name:?}"
        )))
    }
}

/// Validates a user table name: an identifier that does not start with `_`.
/// Names starting with `_` are reserved for the system (e.g. `_default`).
pub fn validate_user_table(name: &str) -> Result<(), AtlantisError> {
    validate_ident("table", name)?;
    if name.starts_with('_') {
        return Err(AtlantisError::InvalidIdentifier(format!(
            "table names starting with '_' are reserved: {name:?}"
        )));
    }
    Ok(())
}

/// Selects a database inside a namespace.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DatabaseRef {
    pub ns: String,
    pub db: String,
}

impl DatabaseRef {
    /// Creates a validated database reference.
    pub fn new(ns: impl Into<String>, db: impl Into<String>) -> Result<Self, AtlantisError> {
        let (ns, db) = (ns.into(), db.into());
        validate_ident("namespace", &ns)?;
        validate_ident("database", &db)?;
        Ok(DatabaseRef { ns, db })
    }
}

impl Default for DatabaseRef {
    fn default() -> Self {
        DatabaseRef {
            ns: DEFAULT_NAMESPACE.into(),
            db: DEFAULT_DATABASE.into(),
        }
    }
}

impl fmt::Display for DatabaseRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.ns, self.db)
    }
}

/// The key part of a [`RecordId`].
///
/// Keys of different kinds never compare equal. In storage they sort by kind
/// first (integer < string < uuid < ulid < array), then by value.
#[derive(Debug, Clone, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum RecordKey {
    Integer(i64),
    String(String),
    Uuid(Uuid),
    Ulid(Ulid),
    /// A composite key, e.g. `['sensor-1', <datetime>]` for time-series
    /// tables. Elements compare element by element, enabling range scans.
    Array(Vec<Value>),
}

impl RecordKey {
    /// Generates a new ULID key — the default when a record is created
    /// without an explicit key.
    pub fn generate() -> Self {
        RecordKey::Ulid(Ulid::new())
    }

    /// Converts a value into a key. Integers, strings and arrays map
    /// directly; a UUID value becomes a UUID key.
    pub fn from_value(value: &Value) -> Result<Self, AtlantisError> {
        match value {
            Value::Integer(i) => Ok(RecordKey::Integer(*i)),
            Value::String(s) if !s.is_empty() => Ok(RecordKey::String(s.clone())),
            Value::Uuid(u) => Ok(RecordKey::Uuid(*u)),
            Value::Array(items) if !items.is_empty() => Ok(RecordKey::Array(items.clone())),
            other => Err(AtlantisError::InvalidRecordId(format!(
                "a {} value cannot be used as a record key",
                other.type_name()
            ))),
        }
    }
}

fn is_plain_ident(s: &str) -> bool {
    !s.is_empty()
        && !s.as_bytes()[0].is_ascii_digit()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl fmt::Display for RecordKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecordKey::Integer(i) => write!(f, "{i}"),
            // Plain identifiers that would not re-parse as a ULID print bare.
            RecordKey::String(s) if is_plain_ident(s) && s.parse::<Ulid>().is_err() => {
                f.write_str(s)
            }
            RecordKey::String(s) => write!(f, "`{}`", s.replace('`', "``")),
            RecordKey::Uuid(u) => write!(f, "u'{u}'"),
            RecordKey::Ulid(u) => write!(f, "{u}"),
            RecordKey::Array(items) => write!(f, "{}", Value::Array(items.clone())),
        }
    }
}

/// Identifies a record: `table:key`, e.g. `Person:alice`, `Person:42`.
#[derive(Debug, Clone, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct RecordId {
    pub table: String,
    pub key: RecordKey,
}

impl RecordId {
    /// Creates a record id, validating the table name.
    pub fn new(table: impl Into<String>, key: RecordKey) -> Result<Self, AtlantisError> {
        let table = table.into();
        validate_ident("table", &table)?;
        Ok(RecordId { table, key })
    }

    /// Creates a record id with a freshly generated ULID key.
    pub fn generate(table: impl Into<String>) -> Result<Self, AtlantisError> {
        RecordId::new(table, RecordKey::generate())
    }
}

impl fmt::Display for RecordId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.table, self.key)
    }
}

impl FromStr for RecordId {
    type Err = AtlantisError;

    /// Parses `table:key`. The key is read as: an integer if it is all
    /// digits (optionally signed); a UUID if written `u'…'`; a string if
    /// wrapped in backticks; a ULID if it is a valid 26-character ULID;
    /// otherwise a plain string. Array keys cannot be parsed from text.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (table, key) = s.split_once(':').ok_or_else(|| {
            AtlantisError::InvalidRecordId(format!("expected table:key, got {s:?}"))
        })?;
        if key.is_empty() {
            return Err(AtlantisError::InvalidRecordId(format!(
                "empty key in {s:?}"
            )));
        }
        let key = if let Ok(i) = key.parse::<i64>() {
            RecordKey::Integer(i)
        } else if let Some(inner) = key.strip_prefix("u'").and_then(|k| k.strip_suffix('\'')) {
            RecordKey::Uuid(inner.parse()?)
        } else if key.len() >= 2 && key.starts_with('`') && key.ends_with('`') {
            RecordKey::String(key[1..key.len() - 1].replace("``", "`"))
        } else if let Ok(u) = key.parse::<Ulid>() {
            RecordKey::Ulid(u)
        } else {
            RecordKey::String(key.to_string())
        };
        RecordId::new(table, key)
    }
}

/// A stored record: a document plus its identity and secondary labels.
///
/// For graph nodes, `id.table` is the node's first label and `labels` holds
/// the remaining (secondary) labels, kept sorted and de-duplicated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub id: RecordId,
    pub labels: Vec<String>,
    pub data: Object,
}

impl Record {
    /// Creates a record, normalizing the label list.
    pub fn new(id: RecordId, labels: Vec<String>, data: Object) -> Result<Self, AtlantisError> {
        let mut labels = labels;
        labels.iter().try_for_each(|l| validate_ident("label", l))?;
        labels.sort();
        labels.dedup();
        labels.retain(|l| *l != id.table);
        Ok(Record { id, labels, data })
    }

    /// Returns a field value.
    pub fn get(&self, field: &str) -> Option<&Value> {
        self.data.get(field)
    }

    /// Returns `true` if the record carries `label` as its table or a
    /// secondary label.
    pub fn has_label(&self, label: &str) -> bool {
        self.id.table == label
            || self
                .labels
                .binary_search_by(|l| l.as_str().cmp(label))
                .is_ok()
    }
}

/// A relationship stored as a record in an edge table.
///
/// `id.table` is the relationship type. `from` is the start node (`in` in
/// the document model, `startNode(r)` in Cypher) and `to` is the end node
/// (`out`, `endNode(r)`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: RecordId,
    #[serde(rename = "in")]
    pub from: RecordId,
    #[serde(rename = "out")]
    pub to: RecordId,
    pub data: Object,
}

impl Edge {
    /// The relationship type (the edge table name).
    pub fn rel_type(&self) -> &str {
        &self.id.table
    }

    /// Returns the edge as a document with `id`, `in` and `out` fields.
    pub fn to_object(&self) -> Object {
        let mut obj = self.data.clone();
        obj.insert("id".into(), Value::RecordId(self.id.clone()));
        obj.insert("in".into(), Value::RecordId(self.from.clone()));
        obj.insert("out".into(), Value::RecordId(self.to.clone()));
        obj
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ident_rules() {
        assert!(validate_ident("table", "Person").is_ok());
        assert!(validate_ident("table", "WORKS_AT").is_ok());
        assert!(validate_ident("table", "").is_err());
        assert!(validate_ident("table", "9lives").is_err());
        assert!(validate_ident("table", "has space").is_err());
        assert!(validate_user_table("_default").is_err());
        assert!(validate_ident("table", DEFAULT_TABLE).is_ok());
    }

    #[test]
    fn record_id_display_and_parse() {
        let cases = [
            ("Person:alice", RecordKey::String("alice".into())),
            ("Person:42", RecordKey::Integer(42)),
            ("Person:-7", RecordKey::Integer(-7)),
            (
                "Person:`hello world`",
                RecordKey::String("hello world".into()),
            ),
            ("Person:`42`", RecordKey::String("42".into())),
        ];
        for (text, key) in cases {
            let id: RecordId = text.parse().unwrap();
            assert_eq!(id.table, "Person");
            assert_eq!(id.key, key, "{text}");
            assert_eq!(id.to_string(), text);
        }
    }

    #[test]
    fn generated_keys_roundtrip() {
        let id = RecordId::generate("Order").unwrap();
        assert!(matches!(id.key, RecordKey::Ulid(_)));
        assert_eq!(id.to_string().parse::<RecordId>().unwrap(), id);

        let uuid = RecordId::new("Doc", RecordKey::Uuid(Uuid::new_v4())).unwrap();
        assert_eq!(uuid.to_string().parse::<RecordId>().unwrap(), uuid);
    }

    #[test]
    fn record_id_rejects_bad_input() {
        assert!("no-colon".parse::<RecordId>().is_err());
        assert!("Person:".parse::<RecordId>().is_err());
        assert!("bad table:1".parse::<RecordId>().is_err());
        assert!(RecordKey::from_value(&Value::Bool(true)).is_err());
        assert!(RecordKey::from_value(&Value::String(String::new())).is_err());
    }

    #[test]
    fn record_labels_are_normalized() {
        let id = RecordId::new("Person", RecordKey::Integer(1)).unwrap();
        let rec = Record::new(
            id,
            vec!["Admin".into(), "Person".into(), "Admin".into()],
            Object::new(),
        )
        .unwrap();
        assert_eq!(rec.labels, vec!["Admin".to_string()]);
        assert!(rec.has_label("Person"));
        assert!(rec.has_label("Admin"));
        assert!(!rec.has_label("Robot"));
    }

    #[test]
    fn edge_document_shape() {
        let a: RecordId = "Person:alice".parse().unwrap();
        let b: RecordId = "Company:acme".parse().unwrap();
        let edge = Edge {
            id: "WORKS_AT:1".parse().unwrap(),
            from: a.clone(),
            to: b.clone(),
            data: Object::new(),
        };
        let obj = edge.to_object();
        assert_eq!(obj.get("in"), Some(&Value::RecordId(a)));
        assert_eq!(obj.get("out"), Some(&Value::RecordId(b)));
        assert_eq!(edge.rel_type(), "WORKS_AT");
    }
}
