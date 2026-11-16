//! Reading inside values: property access (which loads records), labels,
//! and list/string indexing and slicing.

use atlantis_core::{AtlantisError, Geometry, Record, RecordId, Value};
use atlantis_cypher::ast::Expression;

use super::{int_arg, invalid};
use crate::exec::{Exec, Row};

fn list_index(len: usize, i: i64) -> Option<usize> {
    let idx = if i < 0 { len as i64 + i } else { i };
    (0..len as i64).contains(&idx).then_some(idx as usize)
}

impl Exec<'_> {
    pub(crate) fn load(&mut self, id: &RecordId) -> Result<Option<Record>, AtlantisError> {
        self.tx.get(id)
    }

    /// Property access on any value.
    ///
    /// Takes `base` by value so the property is moved out of a map or a
    /// freshly loaded record instead of cloned.
    pub(crate) fn property(&mut self, base: Value, key: &str) -> Result<Value, AtlantisError> {
        Ok(match base {
            Value::Null => Value::Null,
            Value::Object(mut obj) => obj.remove(key).unwrap_or(Value::Null),
            Value::RecordId(id) => {
                if key == "id" {
                    return Ok(Value::RecordId(id));
                }
                match self.load(&id)? {
                    Some(mut record) => record.data.remove(key).unwrap_or(Value::Null),
                    None => Value::Null,
                }
            }
            Value::Geometry(Geometry::Point(p)) => match key {
                "longitude" | "x" => Value::Float(p.lon),
                "latitude" | "y" => Value::Float(p.lat),
                _ => Value::Null,
            },
            Value::Datetime(d) => {
                let text = d.to_string();
                let num =
                    |r: std::ops::Range<usize>| text.get(r).and_then(|s| s.parse::<i64>().ok());
                match key {
                    "year" => num(0..4).map(Value::Integer).unwrap_or(Value::Null),
                    "month" => num(5..7).map(Value::Integer).unwrap_or(Value::Null),
                    "day" => num(8..10).map(Value::Integer).unwrap_or(Value::Null),
                    "hour" => num(11..13).map(Value::Integer).unwrap_or(Value::Null),
                    "minute" => num(14..16).map(Value::Integer).unwrap_or(Value::Null),
                    "second" => num(17..19).map(Value::Integer).unwrap_or(Value::Null),
                    "epochMillis" => Value::Integer(d.0.div_euclid(1_000_000)),
                    _ => Value::Null,
                }
            }
            other => {
                return Err(invalid(format!(
                    "cannot read property {key} of a {}",
                    other.type_name()
                )))
            }
        })
    }

    /// The labels of a node: its table first, then secondary labels.
    pub(crate) fn labels_of(&mut self, id: &RecordId) -> Result<Vec<String>, AtlantisError> {
        Ok(match self.load(id)? {
            Some(record) => std::iter::once(record.id.table.clone())
                .chain(record.labels)
                .collect(),
            None => Vec::new(),
        })
    }

    /// `base[index]`: a list element, a character, or a property by name.
    pub(super) fn index(&mut self, base: Value, index: Value) -> Result<Value, AtlantisError> {
        Ok(match (base, index) {
            (Value::Null, _) | (_, Value::Null) => Value::Null,
            (Value::Array(mut items), index) => {
                let i = int_arg(&index, "list index")?.unwrap_or(0);
                match list_index(items.len(), i) {
                    Some(i) => items.swap_remove(i),
                    None => Value::Null,
                }
            }
            (Value::String(s), index) => {
                let i = int_arg(&index, "string index")?.unwrap_or(0);
                list_index(s.chars().count(), i)
                    .and_then(|i| s.chars().nth(i))
                    .map(|c| Value::String(c.to_string()))
                    .unwrap_or(Value::Null)
            }
            (base, Value::String(key)) => self.property(base, &key)?,
            (base, _) => return Err(invalid(format!("cannot index a {}", base.type_name()))),
        })
    }

    /// `base[from..to]` on a list; either bound may be omitted or negative.
    pub(super) fn slice(
        &mut self,
        base: &Expression,
        from: Option<&Expression>,
        to: Option<&Expression>,
        row: &Row,
    ) -> Result<Value, AtlantisError> {
        let base = self.eval(base, row)?;
        let bound =
            |me: &mut Self, e: Option<&Expression>| -> Result<Option<Option<i64>>, AtlantisError> {
                match e {
                    None => Ok(Some(None)),
                    Some(e) => {
                        let v = me.eval(e, row)?;
                        Ok(int_arg(&v, "slice bound")?.map(Some))
                    }
                }
            };
        let (Some(from), Some(to)) = (bound(self, from)?, bound(self, to)?) else {
            return Ok(Value::Null);
        };
        let Value::Array(mut items) = base else {
            return if base.is_null() {
                Ok(Value::Null)
            } else {
                Err(invalid("only lists can be sliced"))
            };
        };
        let len = items.len() as i64;
        let clamp = |i: i64| (if i < 0 { len + i } else { i }).clamp(0, len) as usize;
        let (a, b) = (clamp(from.unwrap_or(0)), clamp(to.unwrap_or(len)));
        Ok(Value::Array(if a < b {
            items.truncate(b);
            items.drain(..a);
            items
        } else {
            Vec::new()
        }))
    }
}
