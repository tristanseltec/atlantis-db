//! Graph functions: record ids, labels, relationship endpoints, properties
//! and paths.

use atlantis_core::{AtlantisError, RecordId, RecordKey, Value};

use super::{arity, first, invalid, unknown};
use crate::exec::Exec;

/// `rid(table, key)`: builds a record id.
pub(super) fn rid(name: &str, args: Vec<Value>) -> Result<Value, AtlantisError> {
    arity(name, &args, 2)?;
    let Value::String(table) = &args[0] else {
        return Err(invalid("rid() needs a table name string"));
    };
    Ok(Value::RecordId(RecordId::new(
        table.clone(),
        RecordKey::from_value(&args[1])?,
    )?))
}

impl Exec<'_> {
    /// Graph functions; `lname` is the lower-cased `name`.
    pub(super) fn graph_function(
        &mut self,
        name: &str,
        lname: &str,
        args: Vec<Value>,
    ) -> Result<Value, AtlantisError> {
        Ok(match lname {
            "id" | "elementid" => {
                arity(name, &args, 1)?;
                match first(args) {
                    Value::RecordId(id) if lname == "elementid" => Value::String(id.to_string()),
                    Value::RecordId(id) => Value::RecordId(id),
                    other => {
                        return Err(invalid(format!(
                            "id() expects a node or relationship, got {}",
                            other.type_name()
                        )))
                    }
                }
            }
            "labels" => {
                arity(name, &args, 1)?;
                let Value::RecordId(id) = first(args) else {
                    return Err(invalid("labels() expects a node"));
                };
                Value::Array(
                    self.labels_of(&id)?
                        .into_iter()
                        .map(Value::String)
                        .collect(),
                )
            }
            "type" => {
                arity(name, &args, 1)?;
                let Value::RecordId(id) = first(args) else {
                    return Err(invalid("type() expects a relationship"));
                };
                Value::String(id.table)
            }
            "startnode" | "endnode" => {
                arity(name, &args, 1)?;
                let Value::RecordId(id) = first(args) else {
                    return Err(invalid(format!("{name}() expects a relationship")));
                };
                match self.tx.get_edge(&id)? {
                    Some(edge) => Value::RecordId(if lname == "startnode" {
                        edge.from
                    } else {
                        edge.to
                    }),
                    None => Value::Null,
                }
            }
            "properties" => match first(args) {
                Value::Object(o) => Value::Object(o),
                Value::RecordId(id) => match self.load(&id)? {
                    Some(r) => Value::Object(r.data),
                    None => Value::Null,
                },
                other => {
                    return Err(invalid(format!(
                        "properties() expects a map or record, got {}",
                        other.type_name()
                    )))
                }
            },
            "keys" => {
                let obj = match first(args) {
                    Value::Object(o) => o,
                    Value::RecordId(id) => self.load(&id)?.map(|r| r.data).unwrap_or_default(),
                    other => {
                        return Err(invalid(format!(
                            "keys() expects a map or record, got {}",
                            other.type_name()
                        )))
                    }
                };
                Value::Array(obj.into_keys().map(Value::String).collect())
            }
            "nodes" | "relationships" => match first(args) {
                Value::Object(mut path) => path.remove(lname).unwrap_or(Value::Null),
                other => {
                    return Err(invalid(format!(
                        "{name}() expects a path, got {}",
                        other.type_name()
                    )))
                }
            },
            "length" => match first(args) {
                Value::Object(path) => match path.get("relationships") {
                    Some(Value::Array(rels)) => Value::Integer(rels.len() as i64),
                    _ => return Err(invalid("length() expects a path, list or string")),
                },
                Value::Array(items) => Value::Integer(items.len() as i64),
                Value::String(s) => Value::Integer(s.chars().count() as i64),
                other => {
                    return Err(invalid(format!(
                        "length() expects a path, got {}",
                        other.type_name()
                    )))
                }
            },
            _ => return Err(unknown(name)),
        })
    }
}
