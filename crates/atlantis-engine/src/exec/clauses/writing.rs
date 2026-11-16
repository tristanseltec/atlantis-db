//! Writing clauses: `CREATE`, `SET`, `REMOVE` and `[DETACH] DELETE`.

use atlantis_core::{AtlantisError, Object, RecordId, Value};
use atlantis_cypher::ast::{Expression, Pattern, RemoveItem, SetItem};

use crate::exec::{eval::invalid, Exec, Row};

impl Exec<'_> {
    /// `CREATE pattern`, once per row; new variables are bound in the row.
    pub(crate) fn create_clause(
        &mut self,
        mut rows: Vec<Row>,
        pattern: &Pattern,
    ) -> Result<Vec<Row>, AtlantisError> {
        for row in &mut rows {
            for path in &pattern.paths {
                self.create_path(path, row)?;
            }
        }
        Ok(rows)
    }

    /// `SET item, ...`, once per row.
    pub(crate) fn set_clause(
        &mut self,
        mut rows: Vec<Row>,
        items: &[SetItem],
    ) -> Result<Vec<Row>, AtlantisError> {
        for row in &mut rows {
            self.apply_set(items, row)?;
        }
        Ok(rows)
    }

    /// `REMOVE item, ...`, once per row.
    pub(crate) fn remove_clause(
        &mut self,
        rows: Vec<Row>,
        items: &[RemoveItem],
    ) -> Result<Vec<Row>, AtlantisError> {
        for row in &rows {
            self.apply_remove(items, row)?;
        }
        Ok(rows)
    }

    /// `[DETACH] DELETE expr, ...`.
    pub(crate) fn delete_clause(
        &mut self,
        rows: Vec<Row>,
        detach: bool,
        exprs: &[Expression],
    ) -> Result<Vec<Row>, AtlantisError> {
        // Collect every target first, delete relationships, then
        // nodes: `DELETE n, r` must not fail on edges that the
        // same clause deletes.
        let mut targets = Vec::new();
        for row in &rows {
            for expr in exprs {
                collect_delete_targets(self.eval(expr, row)?, &mut targets)?;
            }
        }
        let mut nodes = Vec::new();
        for id in targets {
            if self.tx.table(&id.table)?.is_some_and(|t| t.is_edge()) {
                self.tx.delete_edge(&id)?;
            } else {
                nodes.push(id);
            }
        }
        for id in nodes {
            self.tx.delete(&id, detach)?;
        }
        Ok(rows)
    }

    fn target(row: &Row, variable: &str) -> Result<Option<RecordId>, AtlantisError> {
        match row.get(variable) {
            None => Err(invalid(format!("variable `{variable}` is not defined"))),
            Some(Value::Null) => Ok(None),
            Some(Value::RecordId(id)) => Ok(Some(id.clone())),
            Some(other) => Err(invalid(format!(
                "`{variable}` is a {}, not a node or relationship",
                other.type_name()
            ))),
        }
    }

    pub(super) fn apply_set(
        &mut self,
        items: &[SetItem],
        row: &mut Row,
    ) -> Result<(), AtlantisError> {
        for item in items {
            match item {
                SetItem::Property {
                    variable,
                    path,
                    value,
                } => {
                    let value = self.eval(value, row)?;
                    let Some(id) = Self::target(row, variable)? else {
                        continue;
                    };
                    let mut data = self
                        .load(&id)?
                        .ok_or_else(|| AtlantisError::RecordNotFound(id.to_string()))?
                        .data;
                    set_path(&mut data, path, value)?;
                    self.strip_edge_fields(&id, &mut data)?;
                    self.tx.update(&id, data)?;
                }
                SetItem::Replace { variable, value } | SetItem::Merge { variable, value } => {
                    let value = self.eval(value, row)?;
                    let Some(id) = Self::target(row, variable)? else {
                        continue;
                    };
                    let map = match value {
                        Value::Object(map) => map,
                        Value::RecordId(other) => {
                            self.load(&other)?.map(|r| r.data).unwrap_or_default()
                        }
                        Value::Null => Object::new(),
                        other => {
                            return Err(invalid(format!(
                                "SET {variable} needs a map, got {}",
                                other.type_name()
                            )))
                        }
                    };
                    if matches!(item, SetItem::Replace { .. }) {
                        let mut map: Object =
                            map.into_iter().filter(|(_, v)| !v.is_null()).collect();
                        self.strip_edge_fields(&id, &mut map)?;
                        map.remove("id");
                        self.tx.update(&id, map)?;
                    } else {
                        let mut map = map;
                        self.strip_edge_fields(&id, &mut map)?;
                        map.remove("id");
                        self.tx.merge(&id, map)?;
                    }
                }
                SetItem::Labels { variable, labels } => {
                    let Some(id) = Self::target(row, variable)? else {
                        continue;
                    };
                    self.tx.set_labels(&id, labels, &[])?;
                }
            }
        }
        Ok(())
    }

    fn apply_remove(&mut self, items: &[RemoveItem], row: &Row) -> Result<(), AtlantisError> {
        for item in items {
            match item {
                RemoveItem::Property { variable, path } => {
                    let Some(id) = Self::target(row, variable)? else {
                        continue;
                    };
                    let mut data = self
                        .load(&id)?
                        .ok_or_else(|| AtlantisError::RecordNotFound(id.to_string()))?
                        .data;
                    set_path(&mut data, path, Value::Null)?;
                    self.strip_edge_fields(&id, &mut data)?;
                    self.tx.update(&id, data)?;
                }
                RemoveItem::Labels { variable, labels } => {
                    let Some(id) = Self::target(row, variable)? else {
                        continue;
                    };
                    self.tx.set_labels(&id, &[], labels)?;
                }
            }
        }
        Ok(())
    }

    /// Edge documents expose `in`/`out`, which are not writable fields.
    fn strip_edge_fields(&mut self, id: &RecordId, data: &mut Object) -> Result<(), AtlantisError> {
        if self.tx.table(&id.table)?.is_some_and(|t| t.is_edge()) {
            data.remove("in");
            data.remove("out");
        }
        Ok(())
    }
}

/// Flattens a `DELETE` operand into record ids (lists and paths expand).
fn collect_delete_targets(target: Value, out: &mut Vec<RecordId>) -> Result<(), AtlantisError> {
    match target {
        Value::Null => Ok(()),
        Value::RecordId(id) => {
            out.push(id);
            Ok(())
        }
        Value::Array(items) => items
            .into_iter()
            .try_for_each(|v| collect_delete_targets(v, out)),
        Value::Object(mut path) if path.contains_key("relationships") => {
            if let Some(rels) = path.remove("relationships") {
                collect_delete_targets(rels, out)?;
            }
            match path.remove("nodes") {
                Some(nodes) => collect_delete_targets(nodes, out),
                None => Ok(()),
            }
        }
        other => Err(invalid(format!("cannot delete a {}", other.type_name()))),
    }
}

/// Sets (or, with `Null`, removes) a nested field.
fn set_path(data: &mut Object, path: &[String], value: Value) -> Result<(), AtlantisError> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| invalid("empty property path"))?;
    let mut current = data;
    for key in parents {
        let entry = current
            .entry(key.clone())
            .or_insert_with(|| Value::Object(Object::new()));
        if entry.is_null() {
            *entry = Value::Object(Object::new());
        }
        current = match entry {
            Value::Object(obj) => obj,
            other => {
                return Err(invalid(format!(
                    "cannot set a field inside a {}",
                    other.type_name()
                )))
            }
        };
    }
    if value.is_null() {
        current.remove(last);
    } else {
        current.insert(last.clone(), value);
    }
    Ok(())
}
