//! Built-in procedures for `CALL`: `db.vector.search`, `db.labels`,
//! `db.relationshipTypes` and `db.indexes`.

use atlantis_core::{AtlantisError, Value};
use atlantis_storage::VectorIndexDef;

use super::{
    eval::{as_vector, invalid, lowercase},
    Exec,
};
use crate::vector_index::{FieldFilter, VectorQuery};

/// Output column names and rows of a procedure.
type ProcedureOutput = (Vec<String>, Vec<Vec<Value>>);

fn cols(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

impl Exec<'_> {
    /// Runs a procedure. `filter` holds conditions pushed down from the
    /// `YIELD ... WHERE` (only `db.vector.search` receives any).
    pub(super) fn call_procedure(
        &mut self,
        name: &str,
        args: Vec<Value>,
        filter: Vec<FieldFilter>,
    ) -> Result<ProcedureOutput, AtlantisError> {
        match lowercase(name).as_ref() {
            "db.vector.search" => self.vector_search(args, filter),
            "db.labels" => {
                let tables = self.tx.tables()?;
                Ok((
                    cols(&["label"]),
                    tables
                        .into_iter()
                        .filter(|t| !t.is_edge())
                        .map(|t| vec![Value::String(t.name)])
                        .collect(),
                ))
            }
            "db.relationshiptypes" => {
                let tables = self.tx.tables()?;
                Ok((
                    cols(&["relationshipType"]),
                    tables
                        .into_iter()
                        .filter(|t| t.is_edge())
                        .map(|t| vec![Value::String(t.name)])
                        .collect(),
                ))
            }
            "db.indexes" => {
                let defs = self.tx.vector_indexes()?;
                Ok((
                    cols(&["name", "type", "label", "property", "dimension", "metric"]),
                    defs.into_iter()
                        .map(|d| {
                            vec![
                                Value::String(d.name),
                                Value::String("VECTOR".into()),
                                Value::String(d.label),
                                Value::String(d.field),
                                Value::Integer(d.dimension as i64),
                                Value::String(format!("{:?}", d.metric).to_lowercase()),
                            ]
                        })
                        .collect(),
                ))
            }
            other => Err(invalid(format!("unknown procedure {other}"))),
        }
    }

    /// `db.vector.search(index, vector, k[, threshold | {threshold, efSearch}])`.
    fn vector_search(
        &mut self,
        args: Vec<Value>,
        filter: Vec<FieldFilter>,
    ) -> Result<ProcedureOutput, AtlantisError> {
        if !(3..=4).contains(&args.len()) {
            return Err(invalid(
                "db.vector.search(index, vector, k[, threshold | {threshold, efSearch}])",
            ));
        }
        let Value::String(target) = &args[0] else {
            return Err(invalid(
                "db.vector.search: the first argument is an index name",
            ));
        };
        let def =
            match self.tx.vector_index(target)? {
                Some(def) => def,
                // A label is accepted when exactly one index covers it.
                None => {
                    let matching: Vec<VectorIndexDef> = self
                        .tx
                        .vector_indexes()?
                        .into_iter()
                        .filter(|d| &d.label == target)
                        .collect();
                    match matching.as_slice() {
                        [one] => one.clone(),
                        [] => return Err(invalid(format!(
                            "no vector index named {target} (create one with CREATE VECTOR INDEX)"
                        ))),
                        _ => {
                            return Err(invalid(format!(
                                "several vector indexes cover label {target}; use the index name"
                            )))
                        }
                    }
                }
            };
        let query = as_vector(&args[1])
            .ok_or_else(|| invalid("db.vector.search: the second argument must be a vector"))?;
        let k = match &args[2] {
            Value::Integer(k) if *k > 0 => *k as usize,
            other => {
                return Err(invalid(format!(
                    "db.vector.search: k must be a positive integer, got {other}"
                )))
            }
        };
        let (threshold, ef) = match args.get(3) {
            None | Some(Value::Null) => (None, None),
            Some(Value::Object(opts)) => (
                opts.get("threshold").and_then(Value::as_float),
                opts.get("efSearch")
                    .and_then(Value::as_int)
                    .map(|e| e.max(1) as usize),
            ),
            Some(v) => (
                Some(
                    v.as_float()
                        .ok_or_else(|| invalid("threshold must be a number"))?,
                ),
                None,
            ),
        };
        let db = self.tx.database().clone();
        let request = VectorQuery {
            vector: query,
            k,
            ef_search: ef,
            threshold,
            filter,
        };
        let hits = self.vectors.search(self.store, &db, &def, &request)?;
        Ok((
            cols(&["node", "score"]),
            hits.into_iter()
                .map(|h| vec![Value::RecordId(h.id), Value::Float(h.score)])
                .collect(),
        ))
    }
}
